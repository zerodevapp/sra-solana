import { AnchorProvider, Idl, Program, web3 } from "@anchor-lang/core";
import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  assertIsTransactionWithBlockhashLifetime,
  assertIsTransactionWithinSizeLimit,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createTransactionMessage,
  getBase64EncodedWireTransaction,
  pipe,
  setTransactionMessageConfig,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Blockhash,
  type Instruction,
  type SendableTransaction,
  type Transaction,
  type TransactionWithBlockhashLifetime,
} from "@solana/kit";

const idl = require("../idl/sra_handler.json") as Idl;
const MAINNET_GENESIS_HASH = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const MAX_SAFE_BLOCK_HEIGHT = BigInt(Number.MAX_SAFE_INTEGER);

export type SendableV1Transaction = SendableTransaction &
  Transaction &
  TransactionWithBlockhashLifetime;

export type V1LatestBlockhash = Readonly<{
  blockhash: Blockhash;
  lastValidBlockHeight: bigint;
}>;

type FeeConfigAccount = Readonly<{
  admin: web3.PublicKey;
  pendingAdmin: web3.PublicKey;
  feeRecipient: web3.PublicKey;
  bump: number;
}>;

export function decodeFeeConfig(
  program: Pick<Program, "coder">,
  data: Buffer
): FeeConfigAccount {
  return program.coder.accounts.decode<FeeConfigAccount>("feeConfig", data);
}

export function formatRpcError(error: unknown): string {
  return (
    JSON.stringify(error, (_key, value) =>
      typeof value === "bigint" ? value.toString() : value
    ) ?? String(error)
  );
}

export function toSafeBlockHeight(lastValidBlockHeight: bigint): number {
  if (
    lastValidBlockHeight < 0n ||
    lastValidBlockHeight > MAX_SAFE_BLOCK_HEIGHT
  ) {
    throw new RangeError(
      "lastValidBlockHeight must be between 0 and Number.MAX_SAFE_INTEGER"
    );
  }
  return Number(lastValidBlockHeight);
}

export function toKitInstruction(
  instruction: web3.TransactionInstruction
): Instruction {
  return {
    programAddress: address(instruction.programId.toBase58()),
    accounts: instruction.keys.map((account) => ({
      address: address(account.pubkey.toBase58()),
      role: account.isSigner
        ? account.isWritable
          ? AccountRole.WRITABLE_SIGNER
          : AccountRole.READONLY_SIGNER
        : account.isWritable
        ? AccountRole.WRITABLE
        : AccountRole.READONLY,
    })),
    data: new Uint8Array(instruction.data),
  };
}

export async function compileV1Transaction(
  instruction: web3.TransactionInstruction,
  payerBytes: Uint8Array,
  latestBlockhash: V1LatestBlockhash
): Promise<SendableV1Transaction> {
  const payer = await createKeyPairSignerFromBytes(payerBytes);
  const message = pipe(
    createTransactionMessage({ version: 1 }),
    (value) => setTransactionMessageFeePayerSigner(payer, value),
    (value) =>
      setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, value),
    (value) =>
      appendTransactionMessageInstruction(toKitInstruction(instruction), value),
    (value) =>
      setTransactionMessageConfig(
        {
          computeUnitLimit: 400_000,
          loadedAccountsDataSizeLimit: 67_108_864,
        },
        value
      )
  );
  const transaction = await signTransactionMessageWithSigners(message);
  assertIsTransactionWithBlockhashLifetime(transaction);
  assertIsTransactionWithinSizeLimit(transaction);
  return transaction;
}

export async function main() {
  const provider = AnchorProvider.env();
  const program = new Program(idl, provider);
  const payer = provider.wallet.publicKey;
  const [feeConfig, feeConfigBump] = web3.PublicKey.findProgramAddressSync(
    [Buffer.from("fee_config")],
    program.programId
  );

  if ((await provider.connection.getGenesisHash()) !== MAINNET_GENESIS_HASH) {
    throw new Error("RPC is not Solana mainnet");
  }
  if (await provider.connection.getAccountInfo(feeConfig)) {
    throw new Error(`Fee config already exists: ${feeConfig}`);
  }

  const request = program.methods
    .initializeFeeConfig(web3.PublicKey.default)
    .accountsStrict({
      payer,
      admin: payer,
      feeConfig,
      systemProgram: web3.SystemProgram.programId,
    });

  console.log(`Program: ${program.programId}`);
  console.log(`Fee config: ${feeConfig}`);
  console.log(`Admin: ${payer}`);
  const instruction = await request.instruction();
  const rpc = createSolanaRpc(provider.connection.rpcEndpoint);
  const { value: latestBlockhash } = await rpc.getLatestBlockhash().send();
  const transaction = await compileV1Transaction(
    instruction,
    (provider.wallet as { payer: web3.Keypair }).payer.secretKey,
    latestBlockhash
  );
  const wire = getBase64EncodedWireTransaction(transaction);
  const simulation = await rpc
    .simulateTransaction(wire, {
      commitment: "confirmed",
      encoding: "base64",
    })
    .send();

  for (const log of simulation.value.logs ?? []) console.log(log);
  if (simulation.value.err) {
    throw new Error(
      "Simulation failed: " + formatRpcError(simulation.value.err)
    );
  }

  if (!process.argv.includes("--send")) {
    console.log("Simulation succeeded; no transaction sent");
    return;
  }

  const signature = await rpc
    .sendTransaction(wire, {
      encoding: "base64",
      preflightCommitment: "confirmed",
      skipPreflight: false,
    })
    .send();
  const lastValidBlockHeight = toSafeBlockHeight(
    latestBlockhash.lastValidBlockHeight
  );
  const confirmation = await provider.connection.confirmTransaction(
    {
      blockhash: latestBlockhash.blockhash,
      lastValidBlockHeight,
      signature,
    },
    "finalized"
  );
  if (confirmation.value.err) {
    throw new Error(
      "Transaction failed: " + formatRpcError(confirmation.value.err)
    );
  }
  const accountInfo = await provider.connection.getAccountInfo(
    feeConfig,
    "finalized"
  );
  if (!accountInfo || !accountInfo.owner.equals(program.programId)) {
    throw new Error("Fee config post-state verification failed");
  }
  const initialized = decodeFeeConfig(program, accountInfo.data);
  if (
    !initialized.admin.equals(payer) ||
    !initialized.pendingAdmin.equals(web3.PublicKey.default) ||
    !initialized.feeRecipient.equals(web3.PublicKey.default) ||
    initialized.bump !== feeConfigBump
  ) {
    throw new Error("Fee config post-state verification failed");
  }
  console.log(`Signature: ${signature}`);
}

if (require.main === module) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
