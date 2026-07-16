import * as anchor from "@coral-xyz/anchor";
import fs from "fs";

const { Connection, Keypair, PublicKey, SystemProgram, Transaction } =
  anchor.web3;
const idl = require("../idl/sra_handler.json") as anchor.Idl;

export const HANDLER_PROGRAM_ID = new PublicKey(
  "2U8tvBcQjZx8r6RG4r1pnxdG9LfJF4Ya4hZsNGRmhqt9"
);
export const EXPECTED_FEE_CONFIG = new PublicKey(
  "8EsP57fThwiKcGKzb3YeVGRRMZwW7TR1bQ8gAnNkNjLA"
);
export const MAINNET_GENESIS_HASH =
  "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const SEND_ACKNOWLEDGEMENT = "I_UNDERSTAND_FIRST_CALLER_WINS";

interface TransactionExecutionDependencies {
  simulate: (
    transaction: anchor.web3.Transaction
  ) => Promise<{ err: unknown; logs: string[] | null }>;
  send: (rawTransaction: Buffer) => Promise<string>;
  confirm: (signature: string) => Promise<void>;
}

interface TransactionExecutionResult {
  logs: string[];
  signature: string | null;
}

interface FeeConfigState {
  admin: anchor.web3.PublicKey;
  pendingAdmin: anchor.web3.PublicKey;
  feeRecipient: anchor.web3.PublicKey;
}

export function deriveFeeConfig(
  programId: anchor.web3.PublicKey
): anchor.web3.PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("fee_config")],
    programId
  )[0];
}

export function validateStaticTargets(
  idlProgramId: anchor.web3.PublicKey,
  derivedFeeConfig: anchor.web3.PublicKey
): void {
  if (!idlProgramId.equals(HANDLER_PROGRAM_ID)) {
    throw new Error(`Unexpected IDL program address: ${idlProgramId}`);
  }
  if (!derivedFeeConfig.equals(EXPECTED_FEE_CONFIG)) {
    throw new Error(`Unexpected fee-config PDA: ${derivedFeeConfig}`);
  }
}

export function validatePreflightState(
  genesisHash: string,
  programExecutable: boolean,
  feeConfigExists: boolean
): void {
  if (genesisHash !== MAINNET_GENESIS_HASH) {
    throw new Error(`RPC is not Solana mainnet: genesis hash ${genesisHash}`);
  }
  if (!programExecutable) {
    throw new Error(`Handler is not executable: ${HANDLER_PROGRAM_ID}`);
  }
  if (feeConfigExists) {
    throw new Error(`Fee config already exists: ${EXPECTED_FEE_CONFIG}`);
  }
}

export function validateCreatedFeeConfig(
  accountOwner: anchor.web3.PublicKey,
  state: FeeConfigState,
  expectedAdmin: anchor.web3.PublicKey
): void {
  if (!accountOwner.equals(HANDLER_PROGRAM_ID)) {
    throw new Error("Created fee config has the wrong owner");
  }
  if (!state.admin.equals(expectedAdmin)) {
    throw new Error(`Unexpected fee admin: ${state.admin}`);
  }
  if (!state.pendingAdmin.equals(PublicKey.default)) {
    throw new Error(`Unexpected pending admin: ${state.pendingAdmin}`);
  }
  if (!state.feeRecipient.equals(PublicKey.default)) {
    throw new Error(`Unexpected fee recipient: ${state.feeRecipient}`);
  }
}

export function parseShouldSend(args: string[]): boolean {
  const unknownArgs = args.filter((arg) => arg !== "--send");
  if (unknownArgs.length) {
    throw new Error(`Unknown argument(s): ${unknownArgs.join(", ")}`);
  }
  return args.includes("--send");
}

export function assertSendAcknowledged(
  shouldSend: boolean,
  acknowledgement: string | undefined
): void {
  if (shouldSend && acknowledgement !== SEND_ACKNOWLEDGEMENT) {
    throw new Error(
      `Set FEE_CONFIG_INIT_ACK=${SEND_ACKNOWLEDGEMENT} to acknowledge the permissionless first-caller race`
    );
  }
}

export async function simulateThenMaybeSend(
  transaction: anchor.web3.Transaction,
  payer: anchor.web3.Keypair,
  shouldSend: boolean,
  dependencies: TransactionExecutionDependencies
): Promise<TransactionExecutionResult> {
  const simulation = await dependencies.simulate(transaction);
  const logs = simulation.logs ?? [];
  if (simulation.err) {
    throw new Error(
      `Simulation failed: ${JSON.stringify(simulation.err)}\n${logs.join("\n")}`
    );
  }
  if (!shouldSend) {
    return { logs, signature: null };
  }

  transaction.sign(payer);
  const signature = await dependencies.send(transaction.serialize());
  await dependencies.confirm(signature);
  return { logs, signature };
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required environment variable ${name}`);
  }
  return value;
}

function loadKeypair(path: string): anchor.web3.Keypair {
  try {
    const bytes = JSON.parse(fs.readFileSync(path, "utf8"));
    if (!Array.isArray(bytes) || bytes.length !== 64) {
      throw new Error("invalid keypair bytes");
    }
    return Keypair.fromSecretKey(Uint8Array.from(bytes));
  } catch {
    throw new Error("Unable to load FEE_PAYER keypair file");
  }
}

async function run(): Promise<void> {
  const shouldSend = parseShouldSend(process.argv.slice(2));
  assertSendAcknowledged(shouldSend, process.env.FEE_CONFIG_INIT_ACK);
  const rpcUrl = requiredEnv("MAINNET_RPC");
  const payer = loadKeypair(requiredEnv("FEE_PAYER"));
  const connection = new Connection(rpcUrl, {
    commitment: "confirmed",
    ...(process.env.MAINNET_WS ? { wsEndpoint: process.env.MAINNET_WS } : {}),
  });
  const provider = new anchor.AnchorProvider(
    connection,
    new anchor.Wallet(payer),
    { commitment: "confirmed", preflightCommitment: "confirmed" }
  );
  const program = new anchor.Program(idl, provider);
  const idlProgramId = new PublicKey(idl.address);
  const feeConfig = deriveFeeConfig(HANDLER_PROGRAM_ID);
  validateStaticTargets(idlProgramId, feeConfig);

  const genesisHash = await connection.getGenesisHash();
  const programInfo = await connection.getAccountInfo(
    HANDLER_PROGRAM_ID,
    "confirmed"
  );
  const existingFeeConfig = await connection.getAccountInfo(
    feeConfig,
    "confirmed"
  );
  validatePreflightState(
    genesisHash,
    programInfo?.executable === true,
    existingFeeConfig !== null
  );

  console.log("Mainnet fee-config initialization");
  console.log(`Genesis hash: ${genesisHash}`);
  console.log(`Program: ${HANDLER_PROGRAM_ID}`);
  console.log(`Fee config: ${feeConfig}`);
  console.log(`Fee payer/admin: ${payer.publicKey}`);
  console.log(`Fee recipient: ${PublicKey.default}`);
  console.log(
    "Warning: initialization is first-caller-wins; use a trusted private RPC"
  );
  console.log(`Mode: ${shouldSend ? "SIMULATE THEN SEND" : "SIMULATION ONLY"}`);

  const instruction = await program.methods
    .initializeFeeConfig(PublicKey.default)
    .accountsStrict({
      payer: payer.publicKey,
      admin: payer.publicKey,
      feeConfig,
      systemProgram: SystemProgram.programId,
    })
    .instruction();
  const latest = await connection.getLatestBlockhash("finalized");
  const transaction = new Transaction({
    feePayer: payer.publicKey,
    blockhash: latest.blockhash,
    lastValidBlockHeight: latest.lastValidBlockHeight,
  }).add(instruction);

  const execution = await simulateThenMaybeSend(
    transaction,
    payer,
    shouldSend,
    {
      simulate: async (candidate) => {
        const response = await connection.simulateTransaction(candidate);
        return response.value;
      },
      send: async (rawTransaction) =>
        connection.sendRawTransaction(rawTransaction, {
          skipPreflight: false,
          maxRetries: 3,
        }),
      confirm: async (signature) => {
        const confirmation = await connection.confirmTransaction(
          { ...latest, signature },
          "finalized"
        );
        if (confirmation.value.err) {
          throw new Error(
            `Transaction failed: ${JSON.stringify(confirmation.value.err)}`
          );
        }
      },
    }
  );
  for (const log of execution.logs) {
    console.log(log);
  }
  console.log("Simulation succeeded");
  if (!execution.signature) {
    console.log("No transaction sent. Re-run with --send after review.");
    return;
  }

  console.log(`Signature: ${execution.signature}`);

  const accountInfo = await connection.getAccountInfo(feeConfig, "finalized");
  if (!accountInfo) {
    throw new Error("Finalized fee config account is missing");
  }
  const state = program.coder.accounts.decode(
    "feeConfig",
    accountInfo.data
  ) as FeeConfigState;
  validateCreatedFeeConfig(accountInfo.owner, state, payer.publicKey);
  console.log("Finalized fee config verified");
}

if (require.main === module) {
  run().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
