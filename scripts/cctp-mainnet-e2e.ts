import {
  AnchorProvider,
  BN,
  Idl,
  Program,
  Wallet,
  web3,
} from "@anchor-lang/core";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { keccak_256 } from "@noble/hashes/sha3";
import {
  createSolanaRpc,
  getBase64EncodedWireTransaction,
  type Blockhash,
} from "@solana/kit";
import {
  ARBITRUM_CHAIN_ID,
  ARBITRUM_USDC,
  bytes32,
  compileNativeV1Transaction,
  executeCommandStage,
  executeDestinationSettlementWorkflow,
  executeWithSendGate,
  hexAt,
  mergeIrisAttestation,
  objectAt,
  parseCctpConfig,
  parseCctpState,
  planDestinationResumption,
  retryPostStateRead,
  selectDestinationSettlementMode,
  validatePersistedCircleMessage,
  verifyEvmBoundary,
  type CommandStage,
  type AttemptStatus,
  type CctpConfig,
  type CctpState,
  type IrisExpectation,
  type JsonObject,
} from "./cctp-mainnet-e2e-utils";

const handlerIdl = require("../idl/sra_handler.json") as Idl;
const adapterIdl = require("../idl/sra_cctp_adapter.json") as Idl;
const HANDLER_PROGRAM = new web3.PublicKey(
  (handlerIdl as Idl & { address: string }).address
);
const CCTP_ADAPTER_PROGRAM = new web3.PublicKey(
  (adapterIdl as Idl & { address: string }).address
);
const CCTP_MESSAGE_TRANSMITTER = new web3.PublicKey(
  "CCTPV2Sm4AdWt5296sk4P66VBZ7bEhcARwFaaS9YPbeC"
);
const CCTP_TOKEN_MESSENGER = new web3.PublicKey(
  "CCTPV2vPZJS2u2BBsUoscuikbYjnpFmbFsvVuJdgUMQe"
);
const USDC_MINT = new web3.PublicKey(
  "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
);
const TOKEN_PROGRAM = new web3.PublicKey(
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
);
const ASSOCIATED_TOKEN_PROGRAM = new web3.PublicKey(
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
);
const INSTRUCTIONS_SYSVAR = new web3.PublicKey(
  "Sysvar1nstructions1111111111111111111111111"
);
const CCTP_FEE_RECIPIENT = new web3.PublicKey(
  "4BPnUzFDibVcWQ5zzixGodRUHwqDxHYpUPdPYus3Bn56"
);
const MAINNET_GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const SOLANA_CHAIN_ID = "34268394551451";
const SOLANA_CCTP_DOMAIN = 5;
const ARBITRUM_CCTP_DOMAIN = 3;
const NO_FEE_BPS_OVERRIDE = 65_535;

export const CCTP_COMMANDS = [
  "preflight",
  "source-setup",
  "source-deposit",
  "iris-wait",
  "arbitrum-receive",
  "destination-setup",
  "destination-receive-finalize",
  "destination-actions",
  "destination-recover",
] as const;

export type CctpCommand = (typeof CCTP_COMMANDS)[number];

export function parseCliArguments(argv: readonly string[]): Readonly<{
  command: CctpCommand;
  configPath?: string;
  statePath?: string;
  send: boolean;
  direction?: "source" | "destination";
  retry: boolean;
  keystorePath?: string;
  passwordFile?: string;
}> {
  const [commandValue, ...rest] = argv;
  if (!CCTP_COMMANDS.includes(commandValue as CctpCommand)) {
    throw new Error(`unknown command ${commandValue ?? ""}`);
  }
  let configPath: string | undefined;
  let statePath: string | undefined;
  let send = false;
  let direction: "source" | "destination" | undefined;
  let retry = false;
  let keystorePath: string | undefined;
  let passwordFile: string | undefined;
  for (let index = 0; index < rest.length; index++) {
    const flag = rest[index];
    if (flag === "--send") {
      send = true;
      continue;
    }
    if (flag === "--retry") {
      retry = true;
      continue;
    }
    if (
      ![
        "--config",
        "--state",
        "--direction",
        "--keystore",
        "--password-file",
      ].includes(flag)
    ) {
      throw new Error(`unknown flag ${flag}`);
    }
    const value = rest[++index];
    if (!value || value.startsWith("--"))
      throw new Error(`${flag} requires a value`);
    if (flag === "--config") configPath = value;
    if (flag === "--state") statePath = value;
    if (flag === "--keystore") keystorePath = value;
    if (flag === "--password-file") passwordFile = value;
    if (flag === "--direction") {
      if (value !== "source" && value !== "destination") {
        throw new Error("--direction must be source or destination");
      }
      direction = value;
    }
  }
  if (
    commandValue === "arbitrum-receive" &&
    send &&
    (!keystorePath || !passwordFile)
  ) {
    throw new Error(
      "arbitrum-receive --send requires --keystore and --password-file"
    );
  }
  if ((keystorePath === undefined) !== (passwordFile === undefined)) {
    throw new Error("--keystore and --password-file must be provided together");
  }
  if (
    keystorePath &&
    commandValue !== "preflight" &&
    commandValue !== "arbitrum-receive"
  ) {
    throw new Error(
      "wallet flags are only valid for preflight and arbitrum-receive"
    );
  }
  return {
    command: commandValue as CctpCommand,
    configPath,
    statePath,
    send,
    direction,
    retry,
    ...(keystorePath ? { keystorePath } : {}),
    ...(passwordFile ? { passwordFile } : {}),
  };
}

type Runtime = Readonly<{
  config: CctpConfig;
  configPath: string;
  state: CctpState;
  statePath: string;
  payer: web3.Keypair;
  connection: web3.Connection;
  provider: AnchorProvider;
  handler: Program;
  adapter: Program;
}>;

type SolanaPlan = Readonly<{
  command: CctpCommand;
  instructions: readonly Readonly<{
    label: string;
    instruction: web3.TransactionInstruction;
  }>[];
  signers: readonly Readonly<{ role: string; keypair: web3.Keypair }>[];
  addresses: Readonly<Record<string, string>>;
  amount: string;
  recipient: string;
}>;

type CastWalletOptions = Readonly<{
  keystorePath?: string;
  passwordFile?: string;
}>;

export function castWalletArguments(
  account: string,
  wallet: CastWalletOptions
) {
  return wallet.keystorePath && wallet.passwordFile
    ? [
        "--keystore",
        wallet.keystorePath,
        "--password-file",
        wallet.passwordFile,
      ]
    : ["--account", account];
}

function evmAddressFromBytes32(value: string) {
  if (!/^0x0{24}[0-9a-fA-F]{40}$/.test(value)) {
    throw new Error(`${value} is not a zero-left-padded EVM address`);
  }
  return `0x${value.slice(-40)}`;
}

function hex(value: Uint8Array): string {
  return `0x${Buffer.from(value).toString("hex")}`;
}

function u16le(value: number): Buffer {
  const output = Buffer.alloc(2);
  output.writeUInt16LE(value);
  return output;
}

function u32le(value: number): Buffer {
  const output = Buffer.alloc(4);
  output.writeUInt32LE(value);
  return output;
}

function u64le(value: bigint | string | number): Buffer {
  return new BN(value.toString()).toArrayLike(Buffer, "le", 8);
}

function u256be(value: bigint | string): Buffer {
  return new BN(value.toString()).toArrayLike(Buffer, "be", 32);
}

function keccak(value: Uint8Array): Buffer {
  return Buffer.from(keccak_256(value));
}

function pda(seeds: readonly Uint8Array[], program: web3.PublicKey) {
  return web3.PublicKey.findProgramAddressSync(
    seeds.map((seed) => Buffer.from(seed)),
    program
  )[0];
}

function ata(owner: web3.PublicKey, mint = USDC_MINT) {
  return pda(
    [owner.toBuffer(), TOKEN_PROGRAM.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM
  );
}

function loadKeypair(path: string): web3.Keypair {
  const bytes = JSON.parse(readFileSync(path, "utf8"));
  if (!Array.isArray(bytes) || bytes.length !== 64) {
    throw new Error(`keypair ${path} must contain 64 bytes`);
  }
  return web3.Keypair.fromSecretKey(Uint8Array.from(bytes));
}

function readJson(path: string): unknown {
  return JSON.parse(readFileSync(path, "utf8"));
}

function writeState(path: string, state: CctpState) {
  const parsed = parseCctpState(state);
  const temporary = `${path}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(parsed, null, 2)}\n`, "utf8");
  renameSync(temporary, path);
}

function defaultStatePath(configPath: string) {
  return `${configPath.replace(/\.json$/i, "")}-state.json`;
}

function resolveRuntimePaths(
  args: ReturnType<typeof parseCliArguments>
): Readonly<{ configPath: string; statePath: string; state: CctpState }> {
  const requestedStatePath = args.statePath
    ? resolve(args.statePath)
    : undefined;
  const existingState =
    requestedStatePath && existsSync(requestedStatePath)
      ? parseCctpState(readJson(requestedStatePath))
      : ({ version: 1 } as const);
  const configPathValue = args.configPath ?? existingState.configPath;
  if (!configPathValue)
    throw new Error("--config is required until state records configPath");
  const configPath = resolve(configPathValue);
  const statePath = requestedStatePath ?? resolve(defaultStatePath(configPath));
  const state = existsSync(statePath)
    ? parseCctpState(readJson(statePath))
    : ({ version: 1, configPath } as const);
  if (state.configPath && resolve(state.configPath) !== configPath) {
    throw new Error(
      `state configPath ${state.configPath} does not match ${configPath}`
    );
  }
  return { configPath, statePath, state: { ...state, configPath } };
}

function createRuntime(args: ReturnType<typeof parseCliArguments>): Runtime {
  const { configPath, statePath, state } = resolveRuntimePaths(args);
  const config = parseCctpConfig(readJson(configPath));
  const payer = loadKeypair(config.payerKeypair);
  const connection = new web3.Connection(config.rpcUrl, "confirmed");
  const provider = new AnchorProvider(connection, new Wallet(payer), {
    commitment: "confirmed",
    preflightCommitment: "confirmed",
  });
  return {
    config,
    configPath,
    state,
    statePath,
    payer,
    connection,
    provider,
    handler: new Program(handlerIdl, provider),
    adapter: new Program(adapterIdl, provider),
  };
}

async function verifyConfiguredEvmBoundary(
  runtime: Runtime,
  requireSigner: boolean,
  wallet: CastWalletOptions = {}
) {
  return verifyEvmBoundary(
    {
      expectedChainId: BigInt(ARBITRUM_CHAIN_ID),
      ...(requireSigner
        ? { expectedSigner: runtime.config.arbitrum.canaryAddress }
        : {}),
    },
    {
      readChainId: async () =>
        BigInt(
          runCast(["chain-id", "--rpc-url", runtime.config.arbitrum.rpcUrl])
        ),
      ...(requireSigner
        ? {
            resolveSigner: async () =>
              runCast([
                "wallet",
                "address",
                ...castWalletArguments(runtime.config.arbitrum.account, wallet),
              ]),
          }
        : {}),
    }
  );
}

function printSummary(
  runtime: Runtime,
  plan: Readonly<{
    command: CctpCommand;
    signers: readonly (
      | Readonly<{ role: string; keypair: web3.Keypair }>
      | Readonly<{ role: string; address: string }>
    )[];
    addresses: Readonly<Record<string, string>>;
    amount: string;
    recipient: string;
  }>,
  genesis: string,
  evmChainId: bigint,
  instructionOrder: readonly string[]
) {
  const destinationFlow = plan.command.startsWith("destination-");
  console.log(`command: ${plan.command}`);
  console.log(`cluster genesis: ${genesis}`);
  console.log(
    `source chain id: ${destinationFlow ? evmChainId : SOLANA_CHAIN_ID}`
  );
  console.log(
    `destination chain id: ${destinationFlow ? SOLANA_CHAIN_ID : evmChainId}`
  );
  console.log(`handler program: ${HANDLER_PROGRAM}`);
  console.log(`CCTP adapter program: ${CCTP_ADAPTER_PROGRAM}`);
  console.log(`Circle MessageTransmitter: ${CCTP_MESSAGE_TRANSMITTER}`);
  console.log(`Circle TokenMessengerMinter: ${CCTP_TOKEN_MESSENGER}`);
  console.log(`Solana USDC mint: ${USDC_MINT}`);
  console.log(`Arbitrum USDC token: ${ARBITRUM_USDC}`);
  console.log(`amount: ${plan.amount}`);
  console.log(`recipient: ${plan.recipient}`);
  console.log(
    `CCTP finality threshold: ${runtime.config.source.minFinalityThreshold}`
  );
  console.log(`CCTP maximum fee: ${runtime.config.source.maxFee}`);
  console.log("signer roles:");
  for (const signer of plan.signers) {
    console.log(
      `  ${signer.role}: ${
        "keypair" in signer ? signer.keypair.publicKey : signer.address
      }`
    );
  }
  console.log("addresses:");
  for (const [name, value] of Object.entries(plan.addresses)) {
    console.log(`  ${name}: ${value}`);
  }
  console.log("instruction order:");
  instructionOrder.forEach((label, index) =>
    console.log(`  ${index}: ${label}`)
  );
  console.log(`state: ${runtime.statePath}`);
}

function formatRpcError(value: unknown) {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item
  );
}

async function prepareSolanaStage(
  runtime: Runtime,
  plan: SolanaPlan,
  onSignature: (signature: string, lastValidBlockHeight: string) => void,
  verifyPostState: (minContextSlot: number) => Promise<void>
): Promise<CommandStage> {
  const genesis = await runtime.connection.getGenesisHash();
  const evm = await verifyConfiguredEvmBoundary(runtime, false);
  printSummary(
    runtime,
    plan,
    genesis,
    evm.chainId,
    plan.instructions.map((item) => item.label)
  );
  if (genesis !== MAINNET_GENESIS)
    throw new Error("RPC is not Solana mainnet-beta");
  const rpc = createSolanaRpc(runtime.config.rpcUrl);
  const { value: latestBlockhash } = await rpc.getLatestBlockhash().send();
  const compiled = await compileNativeV1Transaction(
    plan.instructions.map((item) => item.instruction),
    plan.signers.map((item) => item.keypair.secretKey),
    latestBlockhash
  );
  console.log(`native v1 size: ${compiled.size}/4096 bytes`);
  console.log(`inline addresses: ${compiled.addressCount}/64`);
  console.log(
    `transaction config: computeUnitLimit=${compiled.config.computeUnitLimit} loadedAccountsDataSizeLimit=${compiled.config.loadedAccountsDataSizeLimit}`
  );
  const wire = getBase64EncodedWireTransaction(compiled.transaction);
  let minContextSlot: number | undefined;
  return {
    simulate: async () => {
      const simulation = await rpc
        .simulateTransaction(wire, {
          commitment: "confirmed",
          encoding: "base64",
          sigVerify: false,
        })
        .send();
      for (const log of simulation.value.logs ?? []) console.log(log);
      if (simulation.value.err) {
        throw new Error(
          `simulation failed: ${formatRpcError(simulation.value.err)}`
        );
      }
    },
    broadcast: async () =>
      String(
        await rpc
          .sendTransaction(wire, {
            encoding: "base64",
            preflightCommitment: "confirmed",
            skipPreflight: false,
          })
          .send()
      ),
    writeArtifact: async (signature) => {
      onSignature(signature, latestBlockhash.lastValidBlockHeight.toString());
    },
    confirm: async (signature) => {
      const confirmation = await runtime.connection.confirmTransaction(
        {
          signature,
          blockhash: latestBlockhash.blockhash,
          lastValidBlockHeight: Number(latestBlockhash.lastValidBlockHeight),
        },
        "finalized"
      );
      if (confirmation.value.err) {
        throw new Error(
          `transaction failed: ${formatRpcError(confirmation.value.err)}`
        );
      }
      minContextSlot = confirmation.context.slot;
    },
    verifyPostState: async () => {
      if (minContextSlot === undefined) {
        throw new Error("transaction confirmation slot is unavailable");
      }
      await verifyPostState(minContextSlot);
    },
  };
}

async function runSolanaPlan(
  runtime: Runtime,
  plan: SolanaPlan,
  send: boolean,
  onSignature: (signature: string, lastValidBlockHeight: string) => void,
  verifyPostState: (minContextSlot: number) => Promise<void>
) {
  const stage = await prepareSolanaStage(
    runtime,
    plan,
    onSignature,
    verifyPostState
  );
  const result = await executeCommandStage(send, stage);
  console.log(
    result.mode === "simulation"
      ? "simulation succeeded; no transaction sent"
      : `confirmed: ${result.id}`
  );
  return result;
}

function sourceValues(runtime: Runtime) {
  const source = runtime.config.source;
  const owner = loadKeypair(source.ownerKeypair);
  const initiator = loadKeypair(source.initiatorKeypair);
  const addressBookRoot = loadKeypair(source.addressBookRootKeypair);
  const recipient = bytes32(source.recipient);
  const targetToken = bytes32(source.targetToken);
  const salt = bytes32(source.salt);
  const handlerData = {
    addressBookRoot: addressBookRoot.publicKey,
    targetChainId: new BN(source.targetChainId),
    recipient: Array.from(recipient),
    targetToken: Array.from(targetToken),
    allowedSlippageBps: source.allowedSlippageBps,
    allowedAdapters: [CCTP_ADAPTER_PROGRAM],
  };
  const serializedHandlerData = Buffer.concat([
    addressBookRoot.publicKey.toBuffer(),
    u64le(source.targetChainId),
    recipient,
    targetToken,
    u16le(source.allowedSlippageBps),
    u32le(1),
    CCTP_ADAPTER_PROGRAM.toBuffer(),
  ]);
  const configHash = keccak(
    Buffer.concat([
      owner.publicKey.toBuffer(),
      HANDLER_PROGRAM.toBuffer(),
      keccak(serializedHandlerData),
    ])
  );
  const sraConfig = pda(
    [Buffer.from("sra"), owner.publicKey.toBuffer(), configHash, salt],
    HANDLER_PROGRAM
  );
  const sraVault = ata(sraConfig);
  const addressBookEntry = pda(
    [
      Buffer.from("address_book"),
      addressBookRoot.publicKey.toBuffer(),
      USDC_MINT.toBuffer(),
      u64le(source.targetChainId),
      targetToken,
    ],
    HANDLER_PROGRAM
  );
  const adapterAuthority = pda(
    [Buffer.from("adapter_authority"), sraConfig.toBuffer()],
    CCTP_ADAPTER_PROGRAM
  );
  return {
    source,
    owner,
    initiator,
    addressBookRoot,
    recipient,
    targetToken,
    salt,
    handlerData,
    configHash,
    sraConfig,
    sraVault,
    addressBookEntry,
    adapterAuthority,
    adapterCustody: ata(adapterAuthority),
  };
}

export async function verifyProgramOwnedAccount(
  connection: web3.Connection,
  account: web3.PublicKey,
  owner: web3.PublicKey,
  label: string,
  minContextSlot?: number
) {
  const info = await retryPostStateRead(
    (visibleMinContextSlot) =>
      connection.getAccountInfo(account, {
        commitment: "finalized",
        ...(visibleMinContextSlot === undefined
          ? {}
          : { minContextSlot: visibleMinContextSlot }),
      }),
    minContextSlot
  );
  if (!info.owner.equals(owner)) {
    throw new Error(`${label} post-state verification failed for ${account}`);
  }
  return info;
}

async function tokenAmountAfterConfirmation(
  connection: web3.Connection,
  tokenAccount: web3.PublicKey,
  label: string,
  minContextSlot: number
) {
  const info = await verifyProgramOwnedAccount(
    connection,
    tokenAccount,
    TOKEN_PROGRAM,
    label,
    minContextSlot
  );
  if (info.data.length < 72) {
    throw new Error(`${label} token account data is truncated`);
  }
  return info.data.readBigUInt64LE(64);
}

async function tokenAmountOrZero(
  connection: web3.Connection,
  tokenAccount: web3.PublicKey
) {
  if (!(await connection.getAccountInfo(tokenAccount))) return 0n;
  return BigInt(
    (await connection.getTokenAccountBalance(tokenAccount)).value.amount
  );
}

function saveRuntimeState(runtime: Runtime, state: CctpState) {
  writeState(runtime.statePath, { ...state, configPath: runtime.configPath });
}

async function sourceSetup(runtime: Runtime, send: boolean) {
  const values = sourceValues(runtime);
  if (runtime.state.source?.setupSignature) {
    throw new Error(
      "source setup signature already recorded; refusing to repeat setup"
    );
  }
  const initializeRoot = await runtime.handler.methods
    .initializeAddressBookRoot()
    .accountsStrict({
      payer: runtime.payer.publicKey,
      owner: values.owner.publicKey,
      addressBookRoot: values.addressBookRoot.publicKey,
      systemProgram: web3.SystemProgram.programId,
    })
    .instruction();
  const initializeSra = await runtime.handler.methods
    .initializeSra(
      Array.from(values.configHash),
      values.handlerData,
      Array.from(values.salt),
      values.initiator.publicKey
    )
    .accountsStrict({
      payer: runtime.payer.publicKey,
      owner: values.owner.publicKey,
      addressBookRoot: values.addressBookRoot.publicKey,
      sraConfig: values.sraConfig,
      systemProgram: web3.SystemProgram.programId,
      initialSourceMint: USDC_MINT,
      initialSourceVault: values.sraVault,
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM,
    })
    .instruction();
  const setEntry = await runtime.handler.methods
    .setAddressBookEntry(
      USDC_MINT,
      new BN(values.source.targetChainId),
      Array.from(values.targetToken),
      values.source.targetDecimals
    )
    .accountsStrict({
      payer: runtime.payer.publicKey,
      owner: values.owner.publicKey,
      addressBookRoot: values.addressBookRoot.publicKey,
      addressBookEntry: values.addressBookEntry,
      systemProgram: web3.SystemProgram.programId,
    })
    .instruction();
  await runSolanaPlan(
    runtime,
    {
      command: "source-setup",
      instructions: [
        { label: "initialize_address_book_root", instruction: initializeRoot },
        { label: "initialize_sra", instruction: initializeSra },
        { label: "set_address_book_entry", instruction: setEntry },
      ],
      signers: [
        { role: "fee payer", keypair: runtime.payer },
        { role: "source owner", keypair: values.owner },
        { role: "address-book root", keypair: values.addressBookRoot },
      ],
      addresses: {
        addressBookRoot: values.addressBookRoot.publicKey.toBase58(),
        sraConfig: values.sraConfig.toBase58(),
        sraVault: values.sraVault.toBase58(),
        addressBookEntry: values.addressBookEntry.toBase58(),
        initiator: values.initiator.publicKey.toBase58(),
      },
      amount: values.source.inputAmount,
      recipient: values.source.recipient,
    },
    send,
    (signature) => {
      saveRuntimeState(runtime, {
        ...runtime.state,
        source: { ...runtime.state.source, setupSignature: signature },
      });
    },
    async (minContextSlot) => {
      const rootInfo = await verifyProgramOwnedAccount(
        runtime.connection,
        values.addressBookRoot.publicKey,
        HANDLER_PROGRAM,
        "address-book root",
        minContextSlot
      );
      const sraInfo = await verifyProgramOwnedAccount(
        runtime.connection,
        values.sraConfig,
        HANDLER_PROGRAM,
        "SRA config",
        minContextSlot
      );
      const entryInfo = await verifyProgramOwnedAccount(
        runtime.connection,
        values.addressBookEntry,
        HANDLER_PROGRAM,
        "address-book entry",
        minContextSlot
      );
      await verifyProgramOwnedAccount(
        runtime.connection,
        values.sraVault,
        TOKEN_PROGRAM,
        "SRA vault",
        minContextSlot
      );
      const root = runtime.handler.coder.accounts.decode<{
        owner: web3.PublicKey;
      }>("addressBookRoot", rootInfo.data);
      const sra = runtime.handler.coder.accounts.decode<{
        owner: web3.PublicKey;
        initiator: web3.PublicKey;
        handlerData: {
          targetChainId: BN;
          recipient: number[];
          targetToken: number[];
          allowedAdapters: web3.PublicKey[];
        };
      }>("sraConfig", sraInfo.data);
      const entry = runtime.handler.coder.accounts.decode<{
        sourceMint: web3.PublicKey;
        targetChainId: BN;
        targetToken: number[];
        targetDecimals: number;
      }>("addressBookEntry", entryInfo.data);
      if (
        !root.owner.equals(values.owner.publicKey) ||
        !sra.owner.equals(values.owner.publicKey) ||
        !sra.initiator.equals(values.initiator.publicKey) ||
        !sra.handlerData.targetChainId.eq(
          new BN(values.source.targetChainId)
        ) ||
        !Buffer.from(sra.handlerData.recipient).equals(values.recipient) ||
        !Buffer.from(sra.handlerData.targetToken).equals(values.targetToken) ||
        sra.handlerData.allowedAdapters.length !== 1 ||
        !sra.handlerData.allowedAdapters[0].equals(CCTP_ADAPTER_PROGRAM) ||
        !entry.sourceMint.equals(USDC_MINT) ||
        !entry.targetChainId.eq(new BN(values.source.targetChainId)) ||
        !Buffer.from(entry.targetToken).equals(values.targetToken) ||
        entry.targetDecimals !== values.source.targetDecimals
      ) {
        throw new Error("source setup post-state values do not match config");
      }
    }
  );
}

async function feeRecipientTokenAccount(runtime: Runtime) {
  const feeConfig = pda([Buffer.from("fee_config")], HANDLER_PROGRAM);
  const info = await runtime.connection.getAccountInfo(feeConfig);
  if (!info) throw new Error(`fee config ${feeConfig} does not exist`);
  const decoded = runtime.handler.coder.accounts.decode<{
    feeRecipient: web3.PublicKey;
  }>("feeConfig", info.data);
  return decoded.feeRecipient.equals(web3.PublicKey.default)
    ? null
    : ata(decoded.feeRecipient);
}

async function sourceDeposit(runtime: Runtime, send: boolean) {
  if (runtime.state.source?.solanaSignature) {
    throw new Error("source burn signature already recorded; never burn again");
  }
  const values = sourceValues(runtime);
  const source = values.source;
  const eventData = web3.Keypair.generate();
  const netAmount =
    BigInt(source.inputAmount) - BigInt(source.protocolFeeAmount);
  const prepareParams = {
    inputAmount: new BN(source.inputAmount),
    outputAmount: Array.from(u256be(source.outputAmount)),
    targetChainId: new BN(source.targetChainId),
    recipient: Array.from(values.recipient),
    targetToken: Array.from(values.targetToken),
    protocolFeeAmount: new BN(source.protocolFeeAmount),
    isSponsor: false,
    protocolFeeBpsOverride: NO_FEE_BPS_OVERRIDE,
  };
  const cctpParams = {
    inputAmount: new BN(netAmount.toString()),
    outputAmount: Array.from(u256be(source.outputAmount)),
    targetChainId: new BN(source.targetChainId),
    recipient: Array.from(values.recipient),
    targetToken: Array.from(values.targetToken),
    maxFee: new BN(source.maxFee),
    minFinalityThreshold: source.minFinalityThreshold,
  };
  const messageTransmitterState = pda(
    [Buffer.from("message_transmitter")],
    CCTP_MESSAGE_TRANSMITTER
  );
  const tokenMessenger = pda(
    [Buffer.from("token_messenger")],
    CCTP_TOKEN_MESSENGER
  );
  const remoteTokenMessenger = pda(
    [Buffer.from("remote_token_messenger"), Buffer.from("3")],
    CCTP_TOKEN_MESSENGER
  );
  const tokenMinter = pda([Buffer.from("token_minter")], CCTP_TOKEN_MESSENGER);
  const localToken = pda(
    [Buffer.from("local_token"), USDC_MINT.toBuffer()],
    CCTP_TOKEN_MESSENGER
  );
  const execute = await runtime.adapter.methods
    .executeDepositCctp(cctpParams)
    .accountsStrict({
      feePayer: runtime.payer.publicKey,
      instructionsSysvar: INSTRUCTIONS_SYSVAR,
      sraConfig: values.sraConfig,
      adapterAuthority: values.adapterAuthority,
      adapterCustody: values.adapterCustody,
      inputMint: USDC_MINT,
      senderAuthority: pda(
        [Buffer.from("sender_authority")],
        CCTP_TOKEN_MESSENGER
      ),
      messageTransmitter: messageTransmitterState,
      tokenMessenger,
      remoteTokenMessenger,
      tokenMinter,
      localToken,
      denylistAccount: pda(
        [Buffer.from("denylist_account"), values.adapterAuthority.toBuffer()],
        CCTP_TOKEN_MESSENGER
      ),
      messageSentEventData: eventData.publicKey,
      messageTransmitterProgram: CCTP_MESSAGE_TRANSMITTER,
      cctpProgram: CCTP_TOKEN_MESSENGER,
      cctpEventAuthority: pda(
        [Buffer.from("__event_authority")],
        CCTP_TOKEN_MESSENGER
      ),
      tokenProgram: TOKEN_PROGRAM,
      systemProgram: web3.SystemProgram.programId,
    })
    .instruction();
  const pairedHash = createHash("sha256").update(execute.data).digest();
  const trigger = await runtime.handler.methods
    .trigger({
      params: prepareParams,
      pairedAdapterIxDataHash: Array.from(pairedHash),
    })
    .accountsStrict({
      feePayer: runtime.payer.publicKey,
      initiator: values.initiator.publicKey,
      sraConfig: values.sraConfig,
      sourceMint: USDC_MINT,
      sraVault: values.sraVault,
      adapterAuthority: values.adapterAuthority,
      adapterCustody: values.adapterCustody,
      adapterProgram: CCTP_ADAPTER_PROGRAM,
      addressBookEntry: values.addressBookEntry,
      feeConfig: pda([Buffer.from("fee_config")], HANDLER_PROGRAM),
      protocolFeeConfig: pda([Buffer.from("protocol_fee")], HANDLER_PROGRAM),
      assetFeeTier: pda(
        [Buffer.from("asset_fee_tier"), USDC_MINT.toBuffer()],
        HANDLER_PROGRAM
      ),
      feeRecipientTokenAccount: await feeRecipientTokenAccount(runtime),
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM,
      systemProgram: web3.SystemProgram.programId,
      instructionsSysvar: INSTRUCTIONS_SYSVAR,
    })
    .instruction();
  const preSra = runtime.handler.coder.accounts.decode<{ depositNonce: BN }>(
    "sraConfig",
    (await runtime.connection.getAccountInfo(values.sraConfig))?.data ??
      Buffer.alloc(0)
  );
  const preVault = BigInt(
    (await runtime.connection.getTokenAccountBalance(values.sraVault)).value
      .amount
  );
  await runSolanaPlan(
    runtime,
    {
      command: "source-deposit",
      instructions: [
        { label: "trigger", instruction: trigger },
        { label: "execute_deposit_cctp", instruction: execute },
      ],
      signers: [
        { role: "fee payer", keypair: runtime.payer },
        { role: "initiator", keypair: values.initiator },
        { role: "Circle message event", keypair: eventData },
      ],
      addresses: {
        sraConfig: values.sraConfig.toBase58(),
        sraVault: values.sraVault.toBase58(),
        adapterAuthority: values.adapterAuthority.toBase58(),
        adapterCustody: values.adapterCustody.toBase58(),
        circleEventAccount: eventData.publicKey.toBase58(),
      },
      amount: source.inputAmount,
      recipient: source.recipient,
    },
    send,
    (signature) => {
      saveRuntimeState(runtime, {
        ...runtime.state,
        source: {
          ...runtime.state.source,
          solanaSignature: signature,
          circleEventAccount: eventData.publicKey.toBase58(),
        },
      });
    },
    async (minContextSlot) => {
      const sraInfo = await verifyProgramOwnedAccount(
        runtime.connection,
        values.sraConfig,
        HANDLER_PROGRAM,
        "SRA config",
        minContextSlot
      );
      const postSra = runtime.handler.coder.accounts.decode<{
        depositNonce: BN;
      }>("sraConfig", sraInfo.data);
      if (!postSra.depositNonce.eq(preSra.depositNonce.addn(1))) {
        throw new Error("SRA deposit nonce post-state verification failed");
      }
      const postVault = await tokenAmountAfterConfirmation(
        runtime.connection,
        values.sraVault,
        "SRA vault",
        minContextSlot
      );
      if (postVault !== preVault - BigInt(source.inputAmount)) {
        throw new Error("SRA vault post-state verification failed");
      }
      const custody = await tokenAmountAfterConfirmation(
        runtime.connection,
        values.adapterCustody,
        "adapter custody",
        minContextSlot
      );
      if (custody !== 0n) throw new Error("adapter custody is not zero");
      await verifyProgramOwnedAccount(
        runtime.connection,
        eventData.publicKey,
        CCTP_MESSAGE_TRANSMITTER,
        "Circle event account",
        minContextSlot
      );
    }
  );
}

function destinationValues(runtime: Runtime) {
  const destination = runtime.config.destination;
  const owner = loadKeypair(destination.ownerKeypair);
  const recipient = new web3.PublicKey(destination.recipient);
  const executor = new web3.PublicKey(destination.executor);
  const salt = bytes32(destination.salt);
  const actionBytes = Buffer.concat([
    u32le(1),
    Buffer.from([0]),
    recipient.toBuffer(),
    u16le(10_000),
  ]);
  const routeConfigHash = keccak(
    Buffer.concat([
      owner.publicKey.toBuffer(),
      u64le(destination.sourceChainId),
      USDC_MINT.toBuffer(),
      actionBytes,
    ])
  );
  const route = pda(
    [
      Buffer.from("sra_route"),
      owner.publicKey.toBuffer(),
      routeConfigHash,
      salt,
    ],
    HANDLER_PROGRAM
  );
  const executionAuthority = pda(
    [Buffer.from("execution"), route.toBuffer()],
    HANDLER_PROGRAM
  );
  return {
    destination,
    owner,
    recipient,
    executor,
    salt,
    routeConfigHash,
    route,
    routeVault: ata(route),
    executionAuthority,
    executionVault: ata(executionAuthority),
    recipientAta: ata(recipient),
    ownerAta: ata(owner.publicKey),
  };
}

function createAtaIdempotent(
  payer: web3.PublicKey,
  owner: web3.PublicKey,
  tokenAccount = ata(owner)
) {
  return new web3.TransactionInstruction({
    programId: ASSOCIATED_TOKEN_PROGRAM,
    keys: [
      { pubkey: payer, isSigner: true, isWritable: true },
      { pubkey: tokenAccount, isSigner: false, isWritable: true },
      { pubkey: owner, isSigner: false, isWritable: false },
      { pubkey: USDC_MINT, isSigner: false, isWritable: false },
      {
        pubkey: web3.SystemProgram.programId,
        isSigner: false,
        isWritable: false,
      },
      { pubkey: TOKEN_PROGRAM, isSigner: false, isWritable: false },
    ],
    data: Buffer.from([1]),
  });
}

export function validateDestinationTransferAction(
  actionConfig: unknown,
  expectedRecipient: web3.PublicKey
) {
  if (!Array.isArray(actionConfig) || actionConfig.length !== 1) {
    throw new Error("destination route must contain one transfer action");
  }
  const action = objectAt(actionConfig[0], "destination action");
  const transfer = objectAt(
    action.splTransfer,
    "destination action.splTransfer"
  );
  if (
    !(transfer.recipient instanceof web3.PublicKey) ||
    !transfer.recipient.equals(expectedRecipient)
  ) {
    throw new Error("destination transfer recipient does not match config");
  }
  if (transfer.amountBps !== 10_000) {
    throw new Error("destination transfer amountBps must be 10000");
  }
}

export function validateSettlementPostState(
  state: Readonly<{
    amount: bigint;
    receivedInThisCommand: boolean;
    preRouteBalance: bigint;
    postRouteBalance: bigint;
    preExecutionBalance: bigint;
    postExecutionBalance: bigint;
    receiptAmount: bigint;
  }>
) {
  if (state.postExecutionBalance !== state.preExecutionBalance + state.amount) {
    throw new Error(
      "execution vault balance does not include the settled amount"
    );
  }
  const availableRouteBalance =
    state.preRouteBalance + (state.receivedInThisCommand ? state.amount : 0n);
  if (state.postRouteBalance !== availableRouteBalance - state.amount) {
    throw new Error("route vault conservation check failed");
  }
  if (state.receiptAmount !== state.amount) {
    throw new Error("fill receipt amount does not match the settled amount");
  }
}

function updateRuntimeState(
  runtime: Runtime,
  update: (state: CctpState) => CctpState
) {
  const latest = existsSync(runtime.statePath)
    ? parseCctpState(readJson(runtime.statePath))
    : runtime.state;
  saveRuntimeState(runtime, update(latest));
}

async function destinationSetup(runtime: Runtime, send: boolean) {
  if (runtime.state.destination?.setupSignature) {
    throw new Error(
      "destination setup signature already recorded; refusing to repeat setup"
    );
  }
  const values = destinationValues(runtime);
  const createRoute = await runtime.handler.methods
    .createRoute(Array.from(values.routeConfigHash), {
      sourceChainId: new BN(values.destination.sourceChainId),
      expectedMint: USDC_MINT,
      actionConfig: [
        { splTransfer: { recipient: values.recipient, amountBps: 10_000 } },
      ],
      salt: Array.from(values.salt),
      allowedAdapters: [CCTP_ADAPTER_PROGRAM],
      allowedExecutors: [values.executor],
    })
    .accountsStrict({
      payer: runtime.payer.publicKey,
      owner: values.owner.publicKey,
      route: values.route,
      expectedMint: USDC_MINT,
      routeVault: values.routeVault,
      executionAuthority: values.executionAuthority,
      executionVault: values.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM,
      systemProgram: web3.SystemProgram.programId,
    })
    .remainingAccounts([
      { pubkey: CCTP_ADAPTER_PROGRAM, isSigner: false, isWritable: false },
    ])
    .instruction();
  const createRecipientAta = createAtaIdempotent(
    runtime.payer.publicKey,
    values.recipient,
    values.recipientAta
  );
  await runSolanaPlan(
    runtime,
    {
      command: "destination-setup",
      instructions: [
        { label: "create_route", instruction: createRoute },
        { label: "create_recipient_ata", instruction: createRecipientAta },
      ],
      signers: [
        { role: "fee payer", keypair: runtime.payer },
        { role: "destination owner", keypair: values.owner },
      ],
      addresses: {
        route: values.route.toBase58(),
        routeVault: values.routeVault.toBase58(),
        routeVaultBytes32: hex(values.routeVault.toBytes()),
        executionAuthority: values.executionAuthority.toBase58(),
        executionVault: values.executionVault.toBase58(),
        recipientAta: values.recipientAta.toBase58(),
        executor: values.executor.toBase58(),
      },
      amount: values.destination.amount,
      recipient: values.recipient.toBase58(),
    },
    send,
    (signature) => {
      saveRuntimeState(runtime, {
        ...runtime.state,
        destination: {
          ...runtime.state.destination,
          setupSignature: signature,
        },
      });
    },
    async (minContextSlot) => {
      const routeInfo = await verifyProgramOwnedAccount(
        runtime.connection,
        values.route,
        HANDLER_PROGRAM,
        "destination route",
        minContextSlot
      );
      await verifyProgramOwnedAccount(
        runtime.connection,
        values.routeVault,
        TOKEN_PROGRAM,
        "route vault",
        minContextSlot
      );
      await verifyProgramOwnedAccount(
        runtime.connection,
        values.executionVault,
        TOKEN_PROGRAM,
        "execution vault",
        minContextSlot
      );
      await verifyProgramOwnedAccount(
        runtime.connection,
        values.recipientAta,
        TOKEN_PROGRAM,
        "recipient ATA",
        minContextSlot
      );
      const route = runtime.handler.coder.accounts.decode<{
        owner: web3.PublicKey;
        sourceChainId: BN;
        expectedMint: web3.PublicKey;
        actionConfig: unknown;
        allowedAdapters: web3.PublicKey[];
        allowedExecutors: web3.PublicKey[];
        paused: boolean;
      }>("solanaSraRoute", routeInfo.data);
      validateDestinationTransferAction(route.actionConfig, values.recipient);
      if (
        !route.owner.equals(values.owner.publicKey) ||
        !route.sourceChainId.eq(new BN(values.destination.sourceChainId)) ||
        !route.expectedMint.equals(USDC_MINT) ||
        route.allowedAdapters.length !== 1 ||
        !route.allowedAdapters[0].equals(CCTP_ADAPTER_PROGRAM) ||
        route.allowedExecutors.length !== 1 ||
        !route.allowedExecutors[0].equals(values.executor) ||
        route.paused
      ) {
        throw new Error(
          "destination setup post-state values do not match config"
        );
      }
    }
  );
}

function receiveMessageInstruction(
  payer: web3.PublicKey,
  values: ReturnType<typeof destinationValues>,
  nonce: Buffer,
  message: Buffer,
  attestation: Buffer
) {
  const remoteDomain =
    values.destination.sourceChainId === ARBITRUM_CHAIN_ID ? 3 : 0;
  if (remoteDomain === 0)
    throw new Error("unsupported destination source chain");
  const domainSeed = Buffer.from(remoteDomain.toString());
  const discriminator = createHash("sha256")
    .update("global:receive_message")
    .digest()
    .subarray(0, 8);
  const data = Buffer.concat([
    discriminator,
    u32le(message.length),
    message,
    u32le(attestation.length),
    attestation,
  ]);
  const find = (seeds: Uint8Array[], program: web3.PublicKey) =>
    pda(seeds, program);
  return new web3.TransactionInstruction({
    programId: CCTP_MESSAGE_TRANSMITTER,
    keys: [
      { pubkey: payer, isSigner: true, isWritable: true },
      { pubkey: payer, isSigner: true, isWritable: false },
      {
        pubkey: find(
          [
            Buffer.from("message_transmitter_authority"),
            CCTP_TOKEN_MESSENGER.toBuffer(),
          ],
          CCTP_MESSAGE_TRANSMITTER
        ),
        isSigner: false,
        isWritable: false,
      },
      {
        pubkey: find(
          [Buffer.from("message_transmitter")],
          CCTP_MESSAGE_TRANSMITTER
        ),
        isSigner: false,
        isWritable: false,
      },
      {
        pubkey: find(
          [Buffer.from("used_nonce"), nonce],
          CCTP_MESSAGE_TRANSMITTER
        ),
        isSigner: false,
        isWritable: true,
      },
      { pubkey: CCTP_TOKEN_MESSENGER, isSigner: false, isWritable: false },
      {
        pubkey: web3.SystemProgram.programId,
        isSigner: false,
        isWritable: false,
      },
      {
        pubkey: find(
          [Buffer.from("__event_authority")],
          CCTP_MESSAGE_TRANSMITTER
        ),
        isSigner: false,
        isWritable: false,
      },
      { pubkey: CCTP_MESSAGE_TRANSMITTER, isSigner: false, isWritable: false },
      {
        pubkey: find([Buffer.from("token_messenger")], CCTP_TOKEN_MESSENGER),
        isSigner: false,
        isWritable: false,
      },
      {
        pubkey: find(
          [Buffer.from("remote_token_messenger"), domainSeed],
          CCTP_TOKEN_MESSENGER
        ),
        isSigner: false,
        isWritable: false,
      },
      {
        pubkey: find([Buffer.from("token_minter")], CCTP_TOKEN_MESSENGER),
        isSigner: false,
        isWritable: true,
      },
      {
        pubkey: find(
          [Buffer.from("local_token"), USDC_MINT.toBuffer()],
          CCTP_TOKEN_MESSENGER
        ),
        isSigner: false,
        isWritable: true,
      },
      {
        pubkey: find(
          [
            Buffer.from("token_pair"),
            domainSeed,
            bytes32(values.destination.remoteUsdc),
          ],
          CCTP_TOKEN_MESSENGER
        ),
        isSigner: false,
        isWritable: false,
      },
      { pubkey: ata(CCTP_FEE_RECIPIENT), isSigner: false, isWritable: true },
      { pubkey: values.routeVault, isSigner: false, isWritable: true },
      {
        pubkey: find(
          [Buffer.from("custody"), USDC_MINT.toBuffer()],
          CCTP_TOKEN_MESSENGER
        ),
        isSigner: false,
        isWritable: true,
      },
      { pubkey: TOKEN_PROGRAM, isSigner: false, isWritable: false },
      {
        pubkey: find([Buffer.from("__event_authority")], CCTP_TOKEN_MESSENGER),
        isSigner: false,
        isWritable: false,
      },
      { pubkey: CCTP_TOKEN_MESSENGER, isSigner: false, isWritable: false },
    ],
    data,
  });
}

function usedNoncePda(nonce: Buffer) {
  return pda([Buffer.from("used_nonce"), nonce], CCTP_MESSAGE_TRANSMITTER);
}

function verifiedFillPda(nonce: Buffer) {
  return pda(
    [Buffer.from("verified_fill"), CCTP_ADAPTER_PROGRAM.toBuffer(), nonce],
    CCTP_ADAPTER_PROGRAM
  );
}

function fillReceiptPda(route: web3.PublicKey, nonce: Buffer) {
  return pda(
    [Buffer.from("fill_receipt"), route.toBuffer(), nonce],
    HANDLER_PROGRAM
  );
}

async function verifyUsedNonce(runtime: Runtime, nonce: Buffer) {
  if (!(await isUsedNonce(runtime, nonce))) {
    throw new Error("Circle used_nonce is not marked used");
  }
}

async function isUsedNonce(runtime: Runtime, nonce: Buffer) {
  const account = await runtime.connection.getAccountInfo(usedNoncePda(nonce));
  if (!account) return false;
  if (
    !account.owner.equals(CCTP_MESSAGE_TRANSMITTER) ||
    account.data.length < 9
  ) {
    throw new Error("Circle used_nonce account is malformed");
  }
  return account.data[8] === 1;
}

function executionStatusName(value: unknown) {
  const status = objectAt(value, "execution status");
  return Object.keys(status)[0];
}

async function verifyReceiptStatus(
  runtime: Runtime,
  receipt: web3.PublicKey,
  expected: string
) {
  const info = await verifyProgramOwnedAccount(
    runtime.connection,
    receipt,
    HANDLER_PROGRAM,
    "fill receipt"
  );
  const decoded = runtime.handler.coder.accounts.decode<{
    executionStatus: unknown;
    route: web3.PublicKey;
    fillId: number[];
    amount: BN;
  }>("fillReceipt", info.data);
  if (executionStatusName(decoded.executionStatus) !== expected) {
    throw new Error(`fill receipt status is not ${expected}`);
  }
  return decoded;
}

async function isFinalizedReceipt(runtime: Runtime, receipt: web3.PublicKey) {
  const info = await runtime.connection.getAccountInfo(receipt);
  if (!info) return false;
  if (!info.owner.equals(HANDLER_PROGRAM)) {
    throw new Error("fill receipt has an unexpected owner");
  }
  const decoded = runtime.handler.coder.accounts.decode<{
    executionStatus: unknown;
  }>("fillReceipt", info.data);
  return [
    "finalized",
    "actionsSucceeded",
    "actionsFailed",
    "recovered",
  ].includes(executionStatusName(decoded.executionStatus));
}

async function reconcileAttempt(
  runtime: Runtime,
  signature?: string,
  lastValidBlockHeight?: string
): Promise<AttemptStatus> {
  if (!signature) return "none";
  const status = (
    await runtime.connection.getSignatureStatuses([signature], {
      searchTransactionHistory: true,
    })
  ).value[0];
  if (status?.err) return "failed";
  if (
    status?.confirmationStatus === "confirmed" ||
    status?.confirmationStatus === "finalized"
  ) {
    return "confirmed";
  }
  if (
    !status &&
    lastValidBlockHeight !== undefined &&
    BigInt(await runtime.connection.getBlockHeight("confirmed")) >
      BigInt(lastValidBlockHeight)
  ) {
    return "expired";
  }
  return "pending";
}

async function destinationSettlementInstructions(runtime: Runtime) {
  const destinationState = runtime.state.destination;
  if (
    !destinationState?.message ||
    !destinationState.attestation ||
    !destinationState.nonce
  ) {
    throw new Error(
      "destination Iris message, attestation, and nonce are required"
    );
  }
  validateConfiguredCircleMessage(runtime, "destination", destinationState);
  const values = destinationValues(runtime);
  const nonce = bytes32(destinationState.nonce);
  const message = Buffer.from(destinationState.message.slice(2), "hex");
  const attestation = Buffer.from(destinationState.attestation.slice(2), "hex");
  const receive = receiveMessageInstruction(
    runtime.payer.publicKey,
    values,
    nonce,
    message,
    attestation
  );
  const verifiedFill = verifiedFillPda(nonce);
  const fillReceipt = fillReceiptPda(values.route, nonce);
  const verify = await runtime.adapter.methods
    .verifyCctpMint(Array.from(nonce), message, attestation)
    .accountsStrict({
      payer: runtime.payer.publicKey,
      messageTransmitter: pda(
        [Buffer.from("message_transmitter")],
        CCTP_MESSAGE_TRANSMITTER
      ),
      usedNonce: usedNoncePda(nonce),
      route: values.route,
      verifiedFill,
      systemProgram: web3.SystemProgram.programId,
    })
    .instruction();
  const finalize = await runtime.handler.methods
    .finalizeFill(Array.from(nonce), Array.from(values.routeConfigHash))
    .accountsStrict({
      payer: runtime.payer.publicKey,
      route: values.route,
      verifiedFill,
      fillReceipt,
      expectedMint: USDC_MINT,
      routeVault: values.routeVault,
      executionAuthority: values.executionAuthority,
      executionVault: values.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      systemProgram: web3.SystemProgram.programId,
    })
    .instruction();
  return {
    values,
    nonce,
    fillReceipt,
    receive,
    verify,
    finalize,
  };
}

async function destinationReceiveFinalize(runtime: Runtime, send: boolean) {
  const settlement = await destinationSettlementInstructions(runtime);
  const destinationState = runtime.state.destination;
  const resumption = planDestinationResumption({
    usedNonce: await isUsedNonce(runtime, settlement.nonce),
    receiptFinalized: await isFinalizedReceipt(runtime, settlement.fillReceipt),
    receiveAttempt: await reconcileAttempt(
      runtime,
      destinationState?.solanaReceiveSignature,
      destinationState?.solanaReceiveLastValidBlockHeight
    ),
    finalizeAttempt: await reconcileAttempt(
      runtime,
      destinationState?.solanaFinalizeSignature,
      destinationState?.solanaFinalizeLastValidBlockHeight
    ),
  });
  if (resumption === "complete") {
    console.log("destination fill is already finalized");
    return;
  }
  const alreadyReceived = resumption === "finalize-only";
  const amount = BigInt(settlement.values.destination.amount);
  const preRouteBalance = await tokenAmountOrZero(
    runtime.connection,
    settlement.values.routeVault
  );
  const preExecutionBalance = await tokenAmountOrZero(
    runtime.connection,
    settlement.values.executionVault
  );
  const verifyReceivePostState = async () => {
    await verifyUsedNonce(runtime, settlement.nonce);
    const postRouteBalance = await tokenAmountOrZero(
      runtime.connection,
      settlement.values.routeVault
    );
    if (postRouteBalance !== preRouteBalance + amount) {
      throw new Error("route vault did not receive the expected Circle amount");
    }
  };
  const verifySettlementPostState = async (receivedInThisCommand: boolean) => {
    await verifyUsedNonce(runtime, settlement.nonce);
    const receipt = await verifyReceiptStatus(
      runtime,
      settlement.fillReceipt,
      "finalized"
    );
    if (
      !receipt.route.equals(settlement.values.route) ||
      !Buffer.from(receipt.fillId).equals(settlement.nonce)
    ) {
      throw new Error(
        "fill receipt route or fill id does not match the CCTP mint"
      );
    }
    validateSettlementPostState({
      amount,
      receivedInThisCommand,
      preRouteBalance,
      postRouteBalance: await tokenAmountOrZero(
        runtime.connection,
        settlement.values.routeVault
      ),
      preExecutionBalance,
      postExecutionBalance: await tokenAmountOrZero(
        runtime.connection,
        settlement.values.executionVault
      ),
      receiptAmount: BigInt(receipt.amount.toString()),
    });
  };
  const combined = alreadyReceived
    ? [
        { label: "verify_cctp_mint", instruction: settlement.verify },
        { label: "finalize_fill", instruction: settlement.finalize },
      ]
    : [
        { label: "Circle receive_message", instruction: settlement.receive },
        { label: "verify_cctp_mint", instruction: settlement.verify },
        { label: "finalize_fill", instruction: settlement.finalize },
      ];
  const fitsCombined =
    (await selectDestinationSettlementMode(
      combined.map((item) => item.instruction),
      [runtime.payer.secretKey],
      {
        blockhash: "11111111111111111111111111111111" as Blockhash,
        lastValidBlockHeight: 1n,
      }
    )) === "combined";
  if (fitsCombined || alreadyReceived) {
    await runSolanaPlan(
      runtime,
      {
        command: "destination-receive-finalize",
        instructions: combined,
        signers: [{ role: "permissionless payer", keypair: runtime.payer }],
        addresses: {
          route: settlement.values.route.toBase58(),
          routeVault: settlement.values.routeVault.toBase58(),
          usedNonce: usedNoncePda(settlement.nonce).toBase58(),
          verifiedFill: verifiedFillPda(settlement.nonce).toBase58(),
          fillReceipt: settlement.fillReceipt.toBase58(),
        },
        amount: settlement.values.destination.amount,
        recipient: settlement.values.routeVault.toBase58(),
      },
      send,
      (signature, lastValidBlockHeight) => {
        updateRuntimeState(runtime, (state) => ({
          ...state,
          destination: {
            ...state.destination,
            ...(alreadyReceived
              ? {}
              : {
                  solanaReceiveSignature: signature,
                  solanaReceiveLastValidBlockHeight: lastValidBlockHeight,
                }),
            solanaFinalizeSignature: signature,
            solanaFinalizeLastValidBlockHeight: lastValidBlockHeight,
          },
        }));
      },
      async () => verifySettlementPostState(!alreadyReceived)
    );
    return;
  }
  const receiveStage = await prepareSolanaStage(
    runtime,
    {
      command: "destination-receive-finalize",
      instructions: [
        { label: "Circle receive_message", instruction: settlement.receive },
      ],
      signers: [{ role: "permissionless payer", keypair: runtime.payer }],
      addresses: {
        route: settlement.values.route.toBase58(),
        routeVault: settlement.values.routeVault.toBase58(),
        usedNonce: usedNoncePda(settlement.nonce).toBase58(),
      },
      amount: settlement.values.destination.amount,
      recipient: settlement.values.routeVault.toBase58(),
    },
    (signature, lastValidBlockHeight) => {
      updateRuntimeState(runtime, (state) => ({
        ...state,
        destination: {
          ...state.destination,
          solanaReceiveSignature: signature,
          solanaReceiveLastValidBlockHeight: lastValidBlockHeight,
        },
      }));
    },
    verifyReceivePostState
  );
  const prepareFinalize = () =>
    prepareSolanaStage(
      runtime,
      {
        command: "destination-receive-finalize",
        instructions: [
          { label: "verify_cctp_mint", instruction: settlement.verify },
          { label: "finalize_fill", instruction: settlement.finalize },
        ],
        signers: [{ role: "permissionless payer", keypair: runtime.payer }],
        addresses: {
          route: settlement.values.route.toBase58(),
          routeVault: settlement.values.routeVault.toBase58(),
          verifiedFill: verifiedFillPda(settlement.nonce).toBase58(),
          fillReceipt: settlement.fillReceipt.toBase58(),
        },
        amount: settlement.values.destination.amount,
        recipient: settlement.values.routeVault.toBase58(),
      },
      (signature, lastValidBlockHeight) => {
        updateRuntimeState(runtime, (state) => ({
          ...state,
          destination: {
            ...state.destination,
            solanaFinalizeSignature: signature,
            solanaFinalizeLastValidBlockHeight: lastValidBlockHeight,
          },
        }));
      },
      async () => verifySettlementPostState(true)
    );
  const result = await executeDestinationSettlementWorkflow(
    { send, combinedFits: false, alreadyReceived: false },
    {
      combined: receiveStage,
      receive: receiveStage,
      prepareFinalize,
      reportDeferredFinalize: () =>
        console.log(
          "receive_message simulation succeeded; verify_cctp_mint + finalize_fill will be freshly compiled and simulated only after receive_message is confirmed; no transaction sent"
        ),
    }
  );
  if (result.mode === "sent" && "receiveId" in result) {
    console.log(`confirmed receive: ${result.receiveId}`);
    console.log(`confirmed finalize: ${result.finalizeId}`);
  }
}

async function destinationActions(runtime: Runtime, send: boolean) {
  if (runtime.state.destination?.solanaActionsSignature) {
    throw new Error(
      "destination actions signature already recorded; refusing replay"
    );
  }
  const state = runtime.state.destination;
  if (!state?.nonce) throw new Error("destination nonce is required");
  const values = destinationValues(runtime);
  if (!runtime.payer.publicKey.equals(values.executor)) {
    throw new Error(
      `payer ${runtime.payer.publicKey} is not configured executor ${values.executor}`
    );
  }
  const nonce = bytes32(state.nonce);
  const receipt = fillReceiptPda(values.route, nonce);
  const execute = await runtime.handler.methods
    .executeActions(Array.from(nonce), [])
    .accountsStrict({
      executor: runtime.payer.publicKey,
      route: values.route,
      fillReceipt: receipt,
      expectedMint: USDC_MINT,
      executionAuthority: values.executionAuthority,
      executionVault: values.executionVault,
      tokenProgram: TOKEN_PROGRAM,
      instructionsSysvar: INSTRUCTIONS_SYSVAR,
    })
    .remainingAccounts([
      { pubkey: values.recipientAta, isSigner: false, isWritable: true },
    ])
    .instruction();
  const preRecipient = await tokenAmountOrZero(
    runtime.connection,
    values.recipientAta
  );
  await runSolanaPlan(
    runtime,
    {
      command: "destination-actions",
      instructions: [{ label: "execute_actions", instruction: execute }],
      signers: [{ role: "allowed executor", keypair: runtime.payer }],
      addresses: {
        route: values.route.toBase58(),
        fillReceipt: receipt.toBase58(),
        executionVault: values.executionVault.toBase58(),
        recipientAta: values.recipientAta.toBase58(),
      },
      amount: values.destination.amount,
      recipient: values.recipient.toBase58(),
    },
    send,
    (signature) => {
      updateRuntimeState(runtime, (latest) => ({
        ...latest,
        destination: {
          ...latest.destination,
          solanaActionsSignature: signature,
        },
      }));
    },
    async () => {
      await verifyReceiptStatus(runtime, receipt, "actionsSucceeded");
      const balance = BigInt(
        (await runtime.connection.getTokenAccountBalance(values.executionVault))
          .value.amount
      );
      if (balance !== 0n) throw new Error("execution vault is not zero");
      const postRecipient = await tokenAmountOrZero(
        runtime.connection,
        values.recipientAta
      );
      if (postRecipient !== preRecipient + BigInt(values.destination.amount)) {
        throw new Error("destination recipient balance verification failed");
      }
    }
  );
}

async function destinationRecover(
  runtime: Runtime,
  send: boolean,
  retry: boolean
) {
  if (runtime.state.destination?.solanaRecoverSignature) {
    throw new Error(
      "destination recovery signature already recorded; refusing replay"
    );
  }
  const state = runtime.state.destination;
  if (!state?.nonce) throw new Error("destination nonce is required");
  const values = destinationValues(runtime);
  const nonce = bytes32(state.nonce);
  const receipt = fillReceiptPda(values.route, nonce);
  const instruction = retry
    ? await runtime.handler.methods
        .retryActions(Array.from(nonce), [])
        .accountsStrict({
          owner: values.owner.publicKey,
          route: values.route,
          fillReceipt: receipt,
          expectedMint: USDC_MINT,
          executionAuthority: values.executionAuthority,
          executionVault: values.executionVault,
          tokenProgram: TOKEN_PROGRAM,
          instructionsSysvar: INSTRUCTIONS_SYSVAR,
        })
        .remainingAccounts([
          { pubkey: values.recipientAta, isSigner: false, isWritable: true },
        ])
        .instruction()
    : await runtime.handler.methods
        .recoverFunds()
        .accountsStrict({
          owner: values.owner.publicKey,
          route: values.route,
          fillReceipt: receipt,
          expectedMint: USDC_MINT,
          executionAuthority: values.executionAuthority,
          executionVault: values.executionVault,
          ownerTokenAccount: values.ownerAta,
          tokenProgram: TOKEN_PROGRAM,
        })
        .instruction();
  const instructions = retry
    ? [{ label: "retry_actions", instruction }]
    : [
        {
          label: "create_owner_ata",
          instruction: createAtaIdempotent(
            runtime.payer.publicKey,
            values.owner.publicKey,
            values.ownerAta
          ),
        },
        { label: "recover_funds", instruction },
      ];
  const destinationTokenAccount = retry ? values.recipientAta : values.ownerAta;
  const preDestination = await tokenAmountOrZero(
    runtime.connection,
    destinationTokenAccount
  );
  await runSolanaPlan(
    runtime,
    {
      command: "destination-recover",
      instructions,
      signers: [
        { role: "fee payer", keypair: runtime.payer },
        { role: "destination owner", keypair: values.owner },
      ],
      addresses: {
        route: values.route.toBase58(),
        fillReceipt: receipt.toBase58(),
        executionVault: values.executionVault.toBase58(),
        destination: (retry ? values.recipientAta : values.ownerAta).toBase58(),
      },
      amount: values.destination.amount,
      recipient: (retry ? values.recipient : values.owner.publicKey).toBase58(),
    },
    send,
    (signature) => {
      updateRuntimeState(runtime, (latest) => ({
        ...latest,
        destination: {
          ...latest.destination,
          solanaRecoverSignature: signature,
        },
      }));
    },
    async () => {
      await verifyReceiptStatus(
        runtime,
        receipt,
        retry ? "actionsSucceeded" : "recovered"
      );
      const balance = BigInt(
        (await runtime.connection.getTokenAccountBalance(values.executionVault))
          .value.amount
      );
      if (balance !== 0n) throw new Error("execution vault is not zero");
      const postDestination = await tokenAmountOrZero(
        runtime.connection,
        destinationTokenAccount
      );
      if (
        postDestination !==
        preDestination + BigInt(values.destination.amount)
      ) {
        throw new Error("recovery destination balance verification failed");
      }
    }
  );
}

async function preflight(runtime: Runtime, wallet: CastWalletOptions) {
  const source = sourceValues(runtime);
  const destination = destinationValues(runtime);
  const genesis = await runtime.connection.getGenesisHash();
  const evm = await verifyConfiguredEvmBoundary(runtime, true, wallet);
  if (!("signer" in evm)) throw new Error("cast signer verification failed");
  printSummary(
    runtime,
    {
      command: "preflight",
      signers: [
        { role: "fee payer", keypair: runtime.payer },
        { role: "source owner", keypair: source.owner },
        { role: "initiator", keypair: source.initiator },
        { role: "destination owner", keypair: destination.owner },
        {
          role: "Arbitrum signer",
          address: evm.signer,
        },
      ],
      addresses: {
        sraConfig: source.sraConfig.toBase58(),
        sraVault: source.sraVault.toBase58(),
        route: destination.route.toBase58(),
        routeVault: destination.routeVault.toBase58(),
        routeVaultBytes32: hex(destination.routeVault.toBytes()),
        executionVault: destination.executionVault.toBase58(),
      },
      amount: runtime.config.source.inputAmount,
      recipient: runtime.config.source.recipient,
    },
    genesis,
    evm.chainId,
    []
  );
  if (genesis !== MAINNET_GENESIS)
    throw new Error("RPC is not Solana mainnet-beta");
  const accounts = await runtime.connection.getMultipleAccountsInfo([
    HANDLER_PROGRAM,
    CCTP_ADAPTER_PROGRAM,
    CCTP_MESSAGE_TRANSMITTER,
    CCTP_TOKEN_MESSENGER,
    USDC_MINT,
  ]);
  for (let index = 0; index < 4; index++) {
    if (!accounts[index]?.executable) {
      throw new Error(`program account is not executable at index ${index}`);
    }
  }
  if (!accounts[4] || !accounts[4].owner.equals(TOKEN_PROGRAM)) {
    throw new Error("Solana USDC mint verification failed");
  }
  saveRuntimeState(runtime, runtime.state);
  console.log("preflight passed");
}

function irisDirection(runtime: Runtime, requested?: "source" | "destination") {
  if (requested) return requested;
  const sourceReady = Boolean(
    runtime.state.source?.solanaSignature && !runtime.state.source?.attestation
  );
  const destinationReady = Boolean(
    runtime.state.destination?.evmBurnTransactionHash &&
      !runtime.state.destination?.attestation
  );
  if (sourceReady === destinationReady) {
    throw new Error(
      "--direction source or --direction destination is required"
    );
  }
  return sourceReady ? ("source" as const) : ("destination" as const);
}

function validateConfiguredCircleMessage(
  runtime: Runtime,
  direction: "source" | "destination",
  stage = runtime.state[direction]
) {
  if (!stage?.message || !stage.nonce) {
    throw new Error(`${direction} Circle message and nonce are required`);
  }
  const zero = `0x${"00".repeat(32)}`;
  if (direction === "source") {
    validatePersistedCircleMessage(stage.message, {
      sourceDomain: SOLANA_CCTP_DOMAIN,
      destinationDomain: ARBITRUM_CCTP_DOMAIN,
      nonce: stage.nonce,
      burnToken: hex(USDC_MINT.toBytes()),
      mintRecipient: runtime.config.source.recipient,
      amount:
        BigInt(runtime.config.source.inputAmount) -
        BigInt(runtime.config.source.protocolFeeAmount),
      maxFee: 0n,
      feeExecuted: 0n,
      minFinalityThreshold: 2000,
      finalityThresholdExecuted: 2000,
      destinationCaller: zero,
    });
    return;
  }
  const values = destinationValues(runtime);
  validatePersistedCircleMessage(stage.message, {
    sourceDomain: ARBITRUM_CCTP_DOMAIN,
    destinationDomain: SOLANA_CCTP_DOMAIN,
    nonce: stage.nonce,
    burnToken: runtime.config.destination.remoteUsdc,
    mintRecipient: hex(values.routeVault.toBytes()),
    amount: BigInt(runtime.config.destination.amount),
    maxFee: 0n,
    feeExecuted: 0n,
    minFinalityThreshold: 2000,
    finalityThresholdExecuted: 2000,
    messageRecipient: hex(CCTP_TOKEN_MESSENGER.toBytes()),
    destinationCaller: zero,
  });
}

async function irisWait(
  runtime: Runtime,
  requested?: "source" | "destination"
) {
  const direction = irisDirection(runtime, requested);
  const values = destinationValues(runtime);
  const stage = runtime.state[direction];
  if (stage?.message && stage.attestation && stage.nonce) {
    validateConfiguredCircleMessage(runtime, direction, stage);
    console.log(`${direction} Iris attestation already recorded`);
    return;
  }
  const transactionHash =
    direction === "source"
      ? runtime.state.source?.solanaSignature
      : runtime.state.destination?.evmBurnTransactionHash;
  if (!transactionHash) {
    throw new Error(`${direction} burn transaction hash is not recorded`);
  }
  const expectation: IrisExpectation =
    direction === "source"
      ? {
          sourceDomain: SOLANA_CCTP_DOMAIN,
          destinationDomain: ARBITRUM_CCTP_DOMAIN,
          mintRecipient: runtime.config.source.recipient,
          amount: (
            BigInt(runtime.config.source.inputAmount) -
            BigInt(runtime.config.source.protocolFeeAmount)
          ).toString(),
        }
      : {
          sourceDomain: ARBITRUM_CCTP_DOMAIN,
          destinationDomain: SOLANA_CCTP_DOMAIN,
          mintRecipient: hex(values.routeVault.toBytes()),
          amount: runtime.config.destination.amount,
        };
  const sourceDomain =
    direction === "source" ? SOLANA_CCTP_DOMAIN : ARBITRUM_CCTP_DOMAIN;
  const url = `https://iris-api.circle.com/v2/messages/${sourceDomain}?transactionHash=${encodeURIComponent(
    transactionHash
  )}`;
  for (;;) {
    const response = await fetch(url);
    if (!response.ok)
      throw new Error(`Iris request failed: ${response.status}`);
    const payload = (await response.json()) as unknown;
    const messages = objectAt(payload, "Iris response").messages;
    const first = Array.isArray(messages) ? messages[0] : undefined;
    const status =
      typeof first === "object" && first !== null
        ? (first as JsonObject).status
        : undefined;
    if (status === "complete") {
      const merged = mergeIrisAttestation(
        runtime.state,
        direction,
        payload,
        expectation
      );
      validateConfiguredCircleMessage(runtime, direction, merged[direction]);
      saveRuntimeState(runtime, merged);
      console.log(
        `${direction} Iris attestation saved to ${runtime.statePath}`
      );
      return;
    }
    console.log(`Iris status: ${String(status ?? "pending")}; waiting 5000ms`);
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 5_000));
  }
}

export function sanitizeCastEnvironment(env: NodeJS.ProcessEnv) {
  const sanitized = { ...env };
  delete sanitized.ETH_PASSWORD;
  delete sanitized.ETH_KEYSTORE;
  delete sanitized.ETH_KEYSTORE_ACCOUNT;
  return sanitized;
}

function runCast(args: readonly string[]) {
  const result = spawnSync("cast", [...args], {
    encoding: "utf8",
    env: sanitizeCastEnvironment(process.env),
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(
      `cast ${args[0]} failed: ${(result.stderr || result.stdout).trim()}`
    );
  }
  return result.stdout.trim();
}

async function arbitrumReceive(
  runtime: Runtime,
  send: boolean,
  wallet: CastWalletOptions
) {
  const stage = runtime.state.source;
  if (stage?.evmTransactionHash) {
    throw new Error(
      "Arbitrum receive transaction already recorded; refusing replay"
    );
  }
  if (!stage?.message || !stage.attestation || !stage.nonce) {
    throw new Error("source Iris message, attestation, and nonce are required");
  }
  validateConfiguredCircleMessage(runtime, "source", stage);
  const receiveArgs = [
    runtime.config.arbitrum.messageTransmitter,
    "receiveMessage(bytes,bytes)(bool)",
    stage.message,
    stage.attestation,
  ];
  const evm = await verifyConfiguredEvmBoundary(runtime, true, wallet);
  if (!("signer" in evm)) throw new Error("cast signer verification failed");
  const recipient = evmAddressFromBytes32(runtime.config.source.recipient);
  const targetToken = evmAddressFromBytes32(runtime.config.source.targetToken);
  const readRecipientBalance = () =>
    BigInt(
      runCast([
        "call",
        targetToken,
        "balanceOf(address)(uint256)",
        recipient,
        "--rpc-url",
        runtime.config.arbitrum.rpcUrl,
      ]).split(/\s+/)[0]
    );
  const preRecipientBalance = readRecipientBalance();
  const genesis = await runtime.connection.getGenesisHash();
  const source = sourceValues(runtime);
  printSummary(
    runtime,
    {
      command: "arbitrum-receive",
      signers: [
        {
          role: "Arbitrum signer",
          address: evm.signer,
        },
      ],
      addresses: {
        messageTransmitter: runtime.config.arbitrum.messageTransmitter,
        canary: runtime.config.arbitrum.canaryAddress,
        circleEventAccount: stage.circleEventAccount ?? "not recorded",
      },
      amount: (
        BigInt(runtime.config.source.inputAmount) -
        BigInt(runtime.config.source.protocolFeeAmount)
      ).toString(),
      recipient: runtime.config.source.recipient,
    },
    genesis,
    evm.chainId,
    ["cast call receiveMessage", "cast send receiveMessage"]
  );
  console.log(`source SRA: ${source.sraConfig}`);
  const result = await executeWithSendGate(send, {
    simulate: async () => {
      const output = runCast([
        "call",
        ...receiveArgs,
        "--from",
        runtime.config.arbitrum.canaryAddress,
        "--rpc-url",
        runtime.config.arbitrum.rpcUrl,
      ]);
      if (!/true|0x0*1$/i.test(output)) {
        throw new Error(
          `Arbitrum receiveMessage simulation returned ${output}`
        );
      }
    },
    send: async () => {
      const output = runCast([
        "send",
        ...receiveArgs,
        ...castWalletArguments(runtime.config.arbitrum.account, wallet),
        "--rpc-url",
        runtime.config.arbitrum.rpcUrl,
        "--json",
      ]);
      const castResult = JSON.parse(output) as JsonObject;
      const transactionHash = hexAt(
        castResult.transactionHash,
        "cast transactionHash",
        32
      );
      updateRuntimeState(runtime, (latest) => ({
        ...latest,
        source: { ...latest.source, evmTransactionHash: transactionHash },
      }));
      return transactionHash;
    },
    verify: async (transactionHash) => {
      const receipt = JSON.parse(
        runCast([
          "receipt",
          transactionHash,
          "--rpc-url",
          runtime.config.arbitrum.rpcUrl,
          "--json",
        ])
      ) as JsonObject;
      if (![1, "1", "0x1"].includes(receipt.status as number | string)) {
        throw new Error("Arbitrum transaction receipt is not successful");
      }
      const used = runCast([
        "call",
        runtime.config.arbitrum.messageTransmitter,
        "usedNonces(bytes32)(uint256)",
        stage.nonce as string,
        "--rpc-url",
        runtime.config.arbitrum.rpcUrl,
      ]);
      if (BigInt(used.split(/\s+/)[0]) === 0n) {
        throw new Error("Arbitrum MessageTransmitter nonce is not marked used");
      }
      const netAmount =
        BigInt(runtime.config.source.inputAmount) -
        BigInt(runtime.config.source.protocolFeeAmount);
      if (readRecipientBalance() !== preRecipientBalance + netAmount) {
        throw new Error("Arbitrum recipient balance verification failed");
      }
    },
  });
  console.log(
    result.mode === "simulation"
      ? "simulation succeeded; no transaction sent"
      : `confirmed: ${result.id}`
  );
}

export async function main(argv = process.argv.slice(2)) {
  const args = parseCliArguments(argv);
  const runtime = createRuntime(args);
  if (args.command === "preflight") return preflight(runtime, args);
  if (args.command === "source-setup") return sourceSetup(runtime, args.send);
  if (args.command === "source-deposit")
    return sourceDeposit(runtime, args.send);
  if (args.command === "iris-wait") return irisWait(runtime, args.direction);
  if (args.command === "arbitrum-receive") {
    return arbitrumReceive(runtime, args.send, args);
  }
  if (args.command === "destination-setup") {
    return destinationSetup(runtime, args.send);
  }
  if (args.command === "destination-receive-finalize") {
    return destinationReceiveFinalize(runtime, args.send);
  }
  if (args.command === "destination-actions") {
    return destinationActions(runtime, args.send);
  }
  return destinationRecover(runtime, args.send, args.retry);
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
