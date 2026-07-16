/**
 * Minimal mainnet operator commands for the SRA bidirectional canary.
 *
 * Reuses the checked-in IDLs (idl/*.json) and the exact PDA derivations /
 * account ordering pinned by the Surfpool fixtures
 * (programs/sra-handler/tests/{source,dest}_e2e_surfpool.rs).
 *
 * Every command SIMULATES by default and prints its complete route
 * (genesis, program ids, addresses, token/amount, signer roles, instruction
 * order, simulation logs). Broadcasting requires an explicit --send.
 *
 * No airdrops, no surfnet cheat codes, no generated keys: every signer and
 * the address-book root come from operator-provided keypair files.
 *
 * Usage:
 *   yarn mainnet-e2e <command> --config <config.json> [flags]
 *
 * Commands:
 *   source-setup         init address-book root + SRA config + book entry
 *                        (signers: --payer --owner --root-keypair)
 *   destination-setup    create route + recipient USDC ATA
 *                        (signers: --payer --owner)
 *   source-deposit       validate_and_prepare + execute_deposit_across (v0)
 *                        (signers: --payer --owner; optional --alt)
 *   destination-finalize verify_across_fill + finalize_fill (v0)
 *                        (signers: --payer; needs --relay-data; optional --alt)
 *   destination-actions  execute_actions (payer must be allowed executor)
 *                        (signers: --payer; needs --relay-data)
 *   destination-recover  recover_funds to the owner's USDC ATA
 *                        (signers: --payer --owner; needs --relay-data)
 *
 * Config JSON (all frozen canary values; hex fields accept 20- or 32-byte 0x):
 * {
 *   "rpcUrl": "https://api.mainnet-beta.solana.com",
 *   "source": {
 *     "addressBookRoot": "<pubkey, after source-setup>",
 *     "salt": "0x…32",
 *     "targetChainId": 42161,
 *     "targetHandler": "0x…", "targetAdapter": "0x…", "recipient": "0x…",
 *     "targetToken": "0x…", "targetDecimals": 6,
 *     "allowedSlippageBps": 50,
 *     "inputAmount": "1000000", "outputAmount": "999500",
 *     "fillDeadlineOffset": 3600, "exclusivityPeriod": 0,
 *     "exclusiveRelayer": null
 *   },
 *   "destination": {
 *     "owner": "<route owner pubkey>",
 *     "salt": "0x…32",
 *     "sourceChainId": 42161,
 *     "recipient": "<solana pubkey>",
 *     "executor": "<solana pubkey>"
 *   }
 * }
 *
 * Relay-data JSON (--relay-data): authoritative fields from the EVM Across
 * deposit event — never reconstructed by this script:
 * { "depositor": "0x…|base58", "recipient": "<route pubkey>",
 *   "exclusiveRelayer": "0x0|base58", "inputToken": "0x…|base58",
 *   "outputToken": "<USDC mint>", "inputAmount": "0x…32|decimal",
 *   "outputAmount": "decimal", "originChainId": 42161,
 *   "depositId": "0x…32", "fillDeadline": 123, "exclusivityDeadline": 0,
 *   "message": "0x" }
 */

import * as anchor from "@coral-xyz/anchor";
import { keccak_256 } from "@noble/hashes/sha3";
import { sha256 } from "@noble/hashes/sha256";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

const {
  PublicKey,
  Keypair,
  Connection,
  TransactionMessage,
  VersionedTransaction,
  ComputeBudgetProgram,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  SystemProgram,
} = anchor.web3;
type Pubkey = anchor.web3.PublicKey;
type Ix = anchor.web3.TransactionInstruction;
const BN = anchor.BN;

// Mainnet constants (crates/sra-common/src/constants.rs).
const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const ACROSS_SPOKE_POOL = new PublicKey(
  "DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru"
);
const SOLANA_CHAIN_ID = new BN("34268394551451");
const MAINNET_GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const NO_FEE_BPS_OVERRIDE = 65_535;
// SOURCE_CU_BUDGET in tests/common/mod.rs — also used by the dest settle leg.
const CU_BUDGET = 400_000;
const TOKEN_PROGRAM = anchor.utils.token.TOKEN_PROGRAM_ID;
const ATA_PROGRAM = anchor.utils.token.ASSOCIATED_PROGRAM_ID;

// --- small codecs ---

const hex = (b: Uint8Array) => "0x" + Buffer.from(b).toString("hex");
const u16le = (n: number) => new BN(n).toArrayLike(Buffer, "le", 2);
const u32le = (n: number) => new BN(n).toArrayLike(Buffer, "le", 4);
const u64le = (n: anchor.BN) => n.toArrayLike(Buffer, "le", 8);
const u256be = (n: anchor.BN) => n.toArrayLike(Buffer, "be", 32);
const keccak = (b: Buffer) => Buffer.from(keccak_256(b));

/** 0x-hex → 32 bytes, left-padding 20-byte EVM addresses. */
function bytes32(s: string): Buffer {
  const raw = Buffer.from(s.replace(/^0x/, "").padStart(64, "0"), "hex");
  if (raw.length !== 32) throw new Error(`expected ≤32-byte hex, got ${s}`);
  return raw;
}

/** Relay-data pubkey fields arrive as EVM-style 0x-hex or base58. */
const asPubkey = (s: string) =>
  s.startsWith("0x") ? new PublicKey(bytes32(s)) : new PublicKey(s);
const asU256 = (s: string) =>
  s.startsWith("0x") ? bytes32(s) : u256be(new BN(s));

function loadKeypair(p: string): anchor.web3.Keypair {
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf8")))
  );
}

// --- CLI ---

const [, , command, ...rest] = process.argv;
const flags: Record<string, string | boolean> = {};
for (let i = 0; i < rest.length; i++) {
  const f = rest[i];
  if (!f.startsWith("--")) throw new Error(`unexpected argument ${f}`);
  const next = rest[i + 1];
  if (next !== undefined && !next.startsWith("--")) {
    flags[f.slice(2)] = next;
    i++;
  } else flags[f.slice(2)] = true;
}
const flag = (name: string): string | undefined =>
  flags[name] as string | undefined;
function requiredFlag(name: string): string {
  const v = flag(name);
  if (!v) throw new Error(`--${name} is required for ${command}`);
  return v;
}
const SEND = flags["send"] === true;

const COMMANDS = [
  "source-setup",
  "destination-setup",
  "source-deposit",
  "destination-finalize",
  "destination-actions",
  "destination-recover",
];
if (!COMMANDS.includes(command ?? "")) {
  console.error(
    `usage: mainnet-e2e <${COMMANDS.join(
      "|"
    )}> --config <path> [--payer <kp>] [--owner <kp>] [--root-keypair <kp>] [--relay-data <json>] [--alt <pubkey>] [--send]`
  );
  process.exit(1);
}

const config = JSON.parse(fs.readFileSync(requiredFlag("config"), "utf8"));
function req<T>(v: T | undefined, name: string): T {
  if (v === undefined || v === null)
    throw new Error(`config.${name} is required`);
  return v;
}

const payer = loadKeypair(
  flag("payer") ?? path.join(os.homedir(), ".config/solana/id.json")
);
const connection = new Connection(req(config.rpcUrl, "rpcUrl"), "confirmed");
const provider = new anchor.AnchorProvider(
  connection,
  new anchor.Wallet(payer),
  {}
);
const handlerIdl = JSON.parse(
  fs.readFileSync(path.join(__dirname, "../idl/sra_handler.json"), "utf8")
);
const adapterIdl = JSON.parse(
  fs.readFileSync(
    path.join(__dirname, "../idl/sra_across_adapter.json"),
    "utf8"
  )
);
const handler = new anchor.Program(handlerIdl, provider);
const adapter = new anchor.Program(adapterIdl, provider);
const HANDLER_ID = handler.programId;
const ADAPTER_ID = adapter.programId;

// --- PDA derivations (mirrors tests/common/mod.rs) ---

const pda = (seeds: (Buffer | Uint8Array)[], program: Pubkey) =>
  PublicKey.findProgramAddressSync(seeds.map(Buffer.from), program)[0];
const ata = (owner: Pubkey, mint: Pubkey = USDC_MINT) =>
  anchor.utils.token.associatedAddress({ mint, owner });

const feeConfigPda = () => pda([Buffer.from("fee_config")], HANDLER_ID);
const protocolFeePda = () => pda([Buffer.from("protocol_fee")], HANDLER_ID);
const assetFeeTierPda = (mint: Pubkey) =>
  pda([Buffer.from("asset_fee_tier"), mint.toBuffer()], HANDLER_ID);
const acrossStatePda = () =>
  pda([Buffer.from("state"), u64le(new BN(0))], ACROSS_SPOKE_POOL);
const acrossEventAuthorityPda = () =>
  pda([Buffer.from("__event_authority")], ACROSS_SPOKE_POOL);

// --- config → typed params ---

function sourceHandlerData() {
  const s = req(config.source, "source");
  return {
    addressBookRoot: new PublicKey(
      req(s.addressBookRoot, "source.addressBookRoot")
    ),
    targetChainId: new BN(req(s.targetChainId, "source.targetChainId")),
    targetHandler: Array.from(
      bytes32(req(s.targetHandler, "source.targetHandler"))
    ),
    targetAdapter: Array.from(
      bytes32(req(s.targetAdapter, "source.targetAdapter"))
    ),
    recipient: Array.from(bytes32(req(s.recipient, "source.recipient"))),
    targetToken: Array.from(bytes32(req(s.targetToken, "source.targetToken"))),
    allowedSlippageBps: req(s.allowedSlippageBps, "source.allowedSlippageBps"),
    allowedAdapters: [ADAPTER_ID],
  };
}

/** borsh(SourceHandlerData) — field order pinned by crates/sra-common. */
function borshHandlerData(h: ReturnType<typeof sourceHandlerData>): Buffer {
  return Buffer.concat([
    h.addressBookRoot.toBuffer(),
    u64le(h.targetChainId),
    Buffer.from(h.targetHandler),
    Buffer.from(h.targetAdapter),
    Buffer.from(h.recipient),
    Buffer.from(h.targetToken),
    u16le(h.allowedSlippageBps),
    u32le(h.allowedAdapters.length),
    ...h.allowedAdapters.map((a) => a.toBuffer()),
  ]);
}

/** config_hash = keccak(owner ‖ handler_program ‖ keccak(borsh(handler_data))) */
function sraConfigPda(
  owner: Pubkey,
  h: ReturnType<typeof sourceHandlerData>,
  salt: Buffer
) {
  const configHash = keccak(
    Buffer.concat([
      owner.toBuffer(),
      HANDLER_ID.toBuffer(),
      keccak(borshHandlerData(h)),
    ])
  );
  return {
    configHash,
    sraConfig: pda(
      [Buffer.from("sra"), owner.toBuffer(), configHash, salt],
      HANDLER_ID
    ),
  };
}

function destRouteParams() {
  const d = req(config.destination, "destination");
  return {
    owner: new PublicKey(req(d.owner, "destination.owner")),
    salt: bytes32(req(d.salt, "destination.salt")),
    recipient: new PublicKey(req(d.recipient, "destination.recipient")),
    executor: new PublicKey(req(d.executor, "destination.executor")),
    sourceChainId: new BN(req(d.sourceChainId, "destination.sourceChainId")),
  };
}

/** One SplTransfer of 100% to the canary recipient (dest fixture shape). */
function destRoute() {
  const d = destRouteParams();
  // route_config_hash = keccak(owner ‖ chain_id_le ‖ mint ‖ borsh(action_config))
  const actions = Buffer.concat([
    u32le(1),
    Buffer.from([0]),
    d.recipient.toBuffer(),
    u16le(10_000),
  ]);
  const routeConfigHash = keccak(
    Buffer.concat([
      d.owner.toBuffer(),
      u64le(d.sourceChainId),
      USDC_MINT.toBuffer(),
      actions,
    ])
  );
  const route = pda(
    [Buffer.from("sra_route"), d.owner.toBuffer(), routeConfigHash, d.salt],
    HANDLER_ID
  );
  const executionAuthority = pda(
    [Buffer.from("execution"), route.toBuffer()],
    HANDLER_ID
  );
  return {
    ...d,
    routeConfigHash,
    route,
    executionAuthority,
    routeVault: ata(route),
    executionVault: ata(executionAuthority),
  };
}

function relayDataFromFile() {
  const r = JSON.parse(fs.readFileSync(requiredFlag("relay-data"), "utf8"));
  const message = Buffer.from((r.message ?? "0x").replace(/^0x/, ""), "hex");
  return {
    depositor: asPubkey(req(r.depositor, "relay.depositor")),
    recipient: asPubkey(req(r.recipient, "relay.recipient")),
    exclusiveRelayer: asPubkey(
      req(r.exclusiveRelayer, "relay.exclusiveRelayer")
    ),
    inputToken: asPubkey(req(r.inputToken, "relay.inputToken")),
    outputToken: asPubkey(req(r.outputToken, "relay.outputToken")),
    inputAmount: Array.from(asU256(req(r.inputAmount, "relay.inputAmount"))),
    outputAmount: new BN(req(r.outputAmount, "relay.outputAmount")),
    originChainId: new BN(req(r.originChainId, "relay.originChainId")),
    depositId: Array.from(asU256(req(r.depositId, "relay.depositId"))),
    fillDeadline: req(r.fillDeadline, "relay.fillDeadline"),
    exclusivityDeadline: req(
      r.exclusivityDeadline ?? 0,
      "relay.exclusivityDeadline"
    ),
    message,
  };
}

/** Across SVM relay hash: keccak(borsh(relay_data, message→hash) ‖ chain_id_le).
 *  Empty message hashes to 32 zero bytes, NOT keccak("") (relay_hash.rs). */
function relayHashFor(rd: ReturnType<typeof relayDataFromFile>): Buffer {
  const messageHash =
    rd.message.length === 0 ? Buffer.alloc(32) : keccak(rd.message);
  return keccak(
    Buffer.concat([
      rd.depositor.toBuffer(),
      rd.recipient.toBuffer(),
      rd.exclusiveRelayer.toBuffer(),
      rd.inputToken.toBuffer(),
      rd.outputToken.toBuffer(),
      Buffer.from(rd.inputAmount),
      u64le(rd.outputAmount),
      u64le(rd.originChainId),
      Buffer.from(rd.depositId),
      u32le(rd.fillDeadline),
      u32le(rd.exclusivityDeadline),
      messageHash,
      u64le(SOLANA_CHAIN_ID),
    ])
  );
}

// --- simulate / send ---

async function run(
  label: string,
  addresses: Record<string, unknown>,
  ixs: { label: string; ix: Ix }[],
  signers: Record<string, anchor.web3.Keypair>
) {
  const genesis = await connection.getGenesisHash();
  console.log(`\n=== ${label} ===`);
  console.log(`rpc:      ${config.rpcUrl}`);
  console.log(
    `genesis:  ${genesis}${
      genesis === MAINNET_GENESIS ? " (mainnet-beta)" : " (NOT mainnet-beta!)"
    }`
  );
  console.log(
    `programs: handler=${HANDLER_ID} adapter=${ADAPTER_ID} across=${ACROSS_SPOKE_POOL}`
  );
  console.log(`token:    USDC ${USDC_MINT}`);
  console.log("signers:");
  for (const [role, kp] of Object.entries(signers))
    console.log(`  ${role}: ${kp.publicKey}`);
  console.log("addresses:");
  for (const [k, v] of Object.entries(addresses)) console.log(`  ${k}: ${v}`);
  console.log("instruction order:");
  ixs.forEach(({ label: l, ix }, i) =>
    console.log(`  ${i}: ${l} (program ${ix.programId})`)
  );

  let alts: anchor.web3.AddressLookupTableAccount[] = [];
  if (flag("alt")) {
    const table = (
      await connection.getAddressLookupTable(new PublicKey(requiredFlag("alt")))
    ).value;
    if (!table) throw new Error(`lookup table ${flag("alt")} not found`);
    alts = [table];
    console.log(
      `alt:      ${flag("alt")} (${table.state.addresses.length} addresses)`
    );
  }

  const { blockhash, lastValidBlockHeight } =
    await connection.getLatestBlockhash();
  const message = new TransactionMessage({
    payerKey: payer.publicKey,
    recentBlockhash: blockhash,
    instructions: ixs.map((x) => x.ix),
  }).compileToV0Message(alts);
  const tx = new VersionedTransaction(message);
  const unique = new Map(
    Object.values(signers).map((kp) => [kp.publicKey.toBase58(), kp])
  );
  try {
    tx.sign([...unique.values()]);
    console.log(`tx size:  ${tx.serialize().length} bytes (limit 1232)`);
  } catch (err) {
    // Over the 1232-byte packet limit — the fixtures resolve this shape
    // through an on-chain ALT (representative_lookup_addresses).
    console.error(`transaction too large (${err}).`);
    console.error(
      "create + activate an ALT holding the addresses below (solana address-lookup-table create/extend), then re-run with --alt <table>:"
    );
    const lookup = new Set<string>();
    for (const { ix } of ixs)
      for (const m of ix.keys) if (!m.isSigner) lookup.add(m.pubkey.toBase58());
    for (const a of lookup) console.error(`  ${a}`);
    process.exit(1);
  }

  const sim = await connection.simulateTransaction(tx, { sigVerify: false });
  console.log("simulation logs:");
  for (const l of sim.value.logs ?? []) console.log(`  ${l}`);
  console.log(`simulation CUs: ${sim.value.unitsConsumed}`);
  if (sim.value.err) {
    console.error(`SIMULATION FAILED: ${JSON.stringify(sim.value.err)}`);
    process.exit(1);
  }
  if (!SEND) {
    console.log("simulation only — re-run with --send to broadcast.");
    return;
  }
  const sig = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction({
    signature: sig,
    blockhash,
    lastValidBlockHeight,
  });
  console.log(`sent: ${sig}`);
}

// --- commands ---

async function sourceSetup() {
  const owner = loadKeypair(requiredFlag("owner"));
  const root = loadKeypair(requiredFlag("root-keypair"));
  const s = req(config.source, "source");
  const h = { ...sourceHandlerData(), addressBookRoot: root.publicKey };
  const salt = bytes32(req(s.salt, "source.salt"));
  const { configHash, sraConfig } = sraConfigPda(owner.publicKey, h, salt);
  const targetChainId = new BN(s.targetChainId);
  const targetToken = bytes32(s.targetToken);
  const entry = pda(
    [
      Buffer.from("address_book"),
      root.publicKey.toBuffer(),
      USDC_MINT.toBuffer(),
      u64le(targetChainId),
      targetToken,
    ],
    HANDLER_ID
  );

  const initRoot = await handler.methods
    .initializeAddressBookRoot()
    .accountsStrict({
      payer: payer.publicKey,
      owner: owner.publicKey,
      addressBookRoot: root.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  const initSra = await handler.methods
    .initializeSra(Array.from(configHash), h, Array.from(salt))
    .accountsStrict({
      payer: payer.publicKey,
      owner: owner.publicKey,
      addressBookRoot: root.publicKey,
      sraConfig,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  const setEntry = await handler.methods
    .setAddressBookEntry(
      USDC_MINT,
      targetChainId,
      Array.from(targetToken),
      s.targetDecimals ?? 6
    )
    .accountsStrict({
      payer: payer.publicKey,
      owner: owner.publicKey,
      addressBookRoot: root.publicKey,
      addressBookEntry: entry,
      systemProgram: SystemProgram.programId,
    })
    .instruction();

  await run(
    "source-setup",
    {
      addressBookRoot: root.publicKey,
      sraConfig,
      configHash: hex(configHash),
      addressBookEntry: entry,
      ownerUsdcAta: ata(owner.publicKey),
      sraVault: ata(sraConfig),
    },
    [
      { label: "initialize_address_book_root", ix: initRoot },
      { label: "initialize_sra", ix: initSra },
      { label: "set_address_book_entry", ix: setEntry },
    ],
    { payer, owner, addressBookRoot: root }
  );
  console.log(`\nset config.source.addressBookRoot = "${root.publicKey}"`);
}

async function sourceDeposit() {
  const owner = loadKeypair(requiredFlag("owner"));
  const s = req(config.source, "source");
  const h = sourceHandlerData();
  const salt = bytes32(req(s.salt, "source.salt"));
  const { sraConfig } = sraConfigPda(owner.publicKey, h, salt);
  const inputAmount = new BN(req(s.inputAmount, "source.inputAmount"));
  const outputAmount = new BN(req(s.outputAmount, "source.outputAmount"));
  const exclusiveRelayer = s.exclusiveRelayer
    ? new PublicKey(s.exclusiveRelayer)
    : PublicKey.default;
  const fillDeadlineOffset = req(
    s.fillDeadlineOffset ?? 3600,
    "source.fillDeadlineOffset"
  );
  const exclusivityPeriod = req(
    s.exclusivityPeriod ?? 0,
    "source.exclusivityPeriod"
  );

  const mintNonce = pda(
    [Buffer.from("mint_nonce"), sraConfig.toBuffer(), USDC_MINT.toBuffer()],
    HANDLER_ID
  );
  const adapterAuthority = pda(
    [Buffer.from("adapter_authority"), sraConfig.toBuffer()],
    ADAPTER_ID
  );
  const entry = pda(
    [
      Buffer.from("address_book"),
      h.addressBookRoot.toBuffer(),
      USDC_MINT.toBuffer(),
      u64le(h.targetChainId),
      Buffer.from(h.targetToken),
    ],
    HANDLER_ID
  );
  const acrossState = acrossStatePda();
  const acrossVault = ata(acrossState);

  // Across delegate: ["delegate", keccak(borsh(deposit_now args))] — layout
  // pinned by seed_hash.rs; message is always empty in the two-tx model.
  const delegateSeedHash = keccak(
    Buffer.concat([
      adapterAuthority.toBuffer(),
      Buffer.from(h.targetAdapter),
      USDC_MINT.toBuffer(),
      Buffer.from(h.targetToken),
      u64le(inputAmount),
      u256be(outputAmount),
      u64le(h.targetChainId),
      exclusiveRelayer.toBuffer(),
      u32le(fillDeadlineOffset),
      u32le(exclusivityPeriod),
      u32le(0),
    ])
  );
  const delegate = pda(
    [Buffer.from("delegate"), delegateSeedHash],
    ACROSS_SPOKE_POOL
  );

  const shared = {
    outputAmount: Array.from(u256be(outputAmount)),
    targetChainId: h.targetChainId,
    targetHandler: h.targetHandler,
    targetAdapter: h.targetAdapter,
    recipient: h.recipient,
    targetToken: h.targetToken,
  };
  // Zero protocol fee (recipient unset on mainnet) ⇒ net input == gross input.
  const prepareParams = {
    inputAmount,
    ...shared,
    protocolFeeAmount: new BN(0),
    isSponsor: false,
    protocolFeeBpsOverride: NO_FEE_BPS_OVERRIDE,
  };
  const acrossParams = {
    inputAmount,
    ...shared,
    exclusiveRelayer,
    fillDeadlineOffset,
    exclusivityPeriod,
  };

  const executeIx = await adapter.methods
    .executeDepositAcross(acrossParams)
    .accountsStrict({
      feePayer: payer.publicKey,
      instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
      sraConfig,
      mintNonce,
      adapterAuthority,
      adapterCustody: ata(adapterAuthority),
      inputMint: USDC_MINT,
      acrossProgram: ACROSS_SPOKE_POOL,
      acrossState,
      acrossVault,
      acrossEventAuthority: acrossEventAuthorityPda(),
      delegate,
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ATA_PROGRAM,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  const validateIx = await handler.methods
    .validateAndPrepare(h, prepareParams, Array.from(sha256(executeIx.data)))
    .accountsStrict({
      feePayer: payer.publicKey,
      owner: owner.publicKey,
      sraConfig,
      mintNonce,
      sourceMint: USDC_MINT,
      ownerTokenAccount: ata(owner.publicKey),
      sraVault: ata(sraConfig),
      adapterAuthority,
      adapterCustody: ata(adapterAuthority),
      adapterProgram: ADAPTER_ID,
      addressBookRoot: h.addressBookRoot,
      addressBookEntry: entry,
      feeConfig: feeConfigPda(),
      protocolFeeConfig: protocolFeePda(),
      assetFeeTier: assetFeeTierPda(USDC_MINT),
      feeRecipientTokenAccount: null,
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ATA_PROGRAM,
      systemProgram: SystemProgram.programId,
      instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .instruction();

  await run(
    "source-deposit",
    {
      sraConfig,
      mintNonce,
      sraVault: ata(sraConfig),
      adapterAuthority,
      adapterCustody: ata(adapterAuthority),
      addressBookRoot: h.addressBookRoot,
      addressBookEntry: entry,
      ownerUsdcAta: ata(owner.publicKey),
      acrossState,
      acrossVault,
      delegate,
      inputAmount: inputAmount.toString(),
      outputAmount: outputAmount.toString(),
      exclusiveRelayer,
      fillDeadlineOffset,
      exclusivityPeriod,
    },
    [
      {
        label: `set_compute_unit_limit(${CU_BUDGET})`,
        ix: ComputeBudgetProgram.setComputeUnitLimit({ units: CU_BUDGET }),
      },
      { label: "validate_and_prepare", ix: validateIx },
      { label: "execute_deposit_across", ix: executeIx },
    ],
    { payer, owner }
  );
}

async function destinationSetup() {
  const owner = loadKeypair(requiredFlag("owner"));
  const r = destRoute();
  if (!owner.publicKey.equals(r.owner))
    throw new Error(
      `--owner ${owner.publicKey} != config.destination.owner ${r.owner}`
    );

  const params = {
    sourceChainId: r.sourceChainId,
    expectedMint: USDC_MINT,
    actionConfig: [
      { splTransfer: { recipient: r.recipient, amountBps: 10_000 } },
    ],
    salt: Array.from(r.salt),
    allowedAdapters: [ADAPTER_ID],
    allowedExecutors: [r.executor],
  };
  const createRoute = await handler.methods
    .createRoute(Array.from(r.routeConfigHash), params)
    .accountsStrict({
      payer: payer.publicKey,
      owner: owner.publicKey,
      route: r.route,
      expectedMint: USDC_MINT,
      routeVault: r.routeVault,
      executionAuthority: r.executionAuthority,
      executionVault: r.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ATA_PROGRAM,
      systemProgram: SystemProgram.programId,
    })
    // create_route loader-checks each allowed adapter via remaining_accounts.
    .remainingAccounts([
      { pubkey: ADAPTER_ID, isSigner: false, isWritable: false },
    ])
    .instruction();
  // Idempotent ATA create for the transfer recipient (spl-associated-token-account ix 1).
  const recipientAta = ata(r.recipient);
  const createAta: Ix = new anchor.web3.TransactionInstruction({
    programId: ATA_PROGRAM,
    keys: [
      { pubkey: payer.publicKey, isSigner: true, isWritable: true },
      { pubkey: recipientAta, isSigner: false, isWritable: true },
      { pubkey: r.recipient, isSigner: false, isWritable: false },
      { pubkey: USDC_MINT, isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM, isSigner: false, isWritable: false },
    ],
    data: Buffer.from([1]),
  });

  await run(
    "destination-setup",
    {
      route: r.route,
      routeConfigHash: hex(r.routeConfigHash),
      executionAuthority: r.executionAuthority,
      routeVault: r.routeVault,
      executionVault: r.executionVault,
      recipient: r.recipient,
      recipientUsdcAta: recipientAta,
      executor: r.executor,
    },
    [
      { label: "create_route", ix: createRoute },
      { label: "create_recipient_ata (idempotent)", ix: createAta },
    ],
    { payer, owner }
  );
  console.log(
    `\nEVM deposits must use recipient = route PDA ${r.route} (bytes32: ${hex(
      r.route.toBuffer()
    )})`
  );
}

async function destinationFinalize() {
  const r = destRoute();
  const rd = relayDataFromFile();
  if (!rd.recipient.equals(r.route))
    throw new Error(`relay recipient ${rd.recipient} != route ${r.route}`);
  const relayHash = relayHashFor(rd);
  const fillStatus = pda([Buffer.from("fills"), relayHash], ACROSS_SPOKE_POOL);
  const verifiedFill = pda(
    [Buffer.from("verified_fill"), ADAPTER_ID.toBuffer(), relayHash],
    ADAPTER_ID
  );
  const fillReceipt = pda(
    [Buffer.from("fill_receipt"), r.route.toBuffer(), relayHash],
    HANDLER_ID
  );

  const verifyIx = await adapter.methods
    .verifyAcrossFill(Array.from(relayHash), rd)
    .accountsStrict({
      payer: payer.publicKey,
      fillStatus,
      route: r.route,
      verifiedFill,
      acrossProgram: ACROSS_SPOKE_POOL,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  const finalizeIx = await handler.methods
    .finalizeFill(Array.from(relayHash), Array.from(r.routeConfigHash))
    .accountsStrict({
      payer: payer.publicKey,
      route: r.route,
      verifiedFill,
      fillReceipt,
      expectedMint: USDC_MINT,
      routeVault: r.routeVault,
      executionAuthority: r.executionAuthority,
      executionVault: r.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      systemProgram: SystemProgram.programId,
    })
    .instruction();

  await run(
    "destination-finalize",
    {
      route: r.route,
      relayHash: hex(relayHash),
      fillStatus,
      verifiedFill,
      fillReceipt,
      routeVault: r.routeVault,
      executionVault: r.executionVault,
      outputAmount: rd.outputAmount.toString(),
    },
    [
      {
        label: `set_compute_unit_limit(${CU_BUDGET})`,
        ix: ComputeBudgetProgram.setComputeUnitLimit({ units: CU_BUDGET }),
      },
      { label: "verify_across_fill", ix: verifyIx },
      { label: "finalize_fill", ix: finalizeIx },
    ],
    { payer }
  );
}

async function destinationActions() {
  const r = destRoute();
  const relayHash = relayHashFor(relayDataFromFile());
  const fillReceipt = pda(
    [Buffer.from("fill_receipt"), r.route.toBuffer(), relayHash],
    HANDLER_ID
  );
  const recipientAta = ata(r.recipient);

  const executeIx = await handler.methods
    .executeActions(Array.from(relayHash), [])
    .accountsStrict({
      executor: payer.publicKey,
      route: r.route,
      fillReceipt,
      expectedMint: USDC_MINT,
      executionAuthority: r.executionAuthority,
      executionVault: r.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      instructionsSysvar: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    // One writable recipient ATA per SplTransfer action, in action order.
    .remainingAccounts([
      { pubkey: recipientAta, isSigner: false, isWritable: true },
    ])
    .instruction();

  await run(
    "destination-actions",
    {
      route: r.route,
      fillId: hex(relayHash),
      fillReceipt,
      executionVault: r.executionVault,
      recipient: r.recipient,
      recipientUsdcAta: recipientAta,
    },
    [
      {
        label: `set_compute_unit_limit(${CU_BUDGET})`,
        ix: ComputeBudgetProgram.setComputeUnitLimit({ units: CU_BUDGET }),
      },
      { label: "execute_actions", ix: executeIx },
    ],
    { payer }
  );
}

async function destinationRecover() {
  const owner = loadKeypair(requiredFlag("owner"));
  const r = destRoute();
  if (!owner.publicKey.equals(r.owner))
    throw new Error(
      `--owner ${owner.publicKey} != config.destination.owner ${r.owner}`
    );
  const relayHash = relayHashFor(relayDataFromFile());
  const fillReceipt = pda(
    [Buffer.from("fill_receipt"), r.route.toBuffer(), relayHash],
    HANDLER_ID
  );
  const ownerAta = ata(owner.publicKey);

  const recoverIx = await handler.methods
    .recoverFunds()
    .accountsStrict({
      owner: owner.publicKey,
      route: r.route,
      fillReceipt,
      expectedMint: USDC_MINT,
      executionAuthority: r.executionAuthority,
      executionVault: r.executionVault,
      ownerTokenAccount: ownerAta,
      tokenProgram: TOKEN_PROGRAM,
    })
    .instruction();

  await run(
    "destination-recover",
    {
      route: r.route,
      fillId: hex(relayHash),
      fillReceipt,
      executionVault: r.executionVault,
      ownerUsdcAta: ownerAta,
    },
    [{ label: "recover_funds", ix: recoverIx }],
    { payer, owner }
  );
}

const commands: Record<string, () => Promise<void>> = {
  "source-setup": sourceSetup,
  "destination-setup": destinationSetup,
  "source-deposit": sourceDeposit,
  "destination-finalize": destinationFinalize,
  "destination-actions": destinationActions,
  "destination-recover": destinationRecover,
};

commands[command]().catch((err) => {
  console.error(err);
  process.exit(1);
});
