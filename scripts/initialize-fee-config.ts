import { AnchorProvider, Idl, Program, web3 } from "@coral-xyz/anchor";

const idl = require("../idl/sra_handler.json") as Idl;
const MAINNET_GENESIS_HASH = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

async function main() {
  const provider = AnchorProvider.env();
  const program = new Program(idl, provider);
  const payer = provider.wallet.publicKey;
  const [feeConfig] = web3.PublicKey.findProgramAddressSync(
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
  console.log((await request.simulate()).raw.join("\n"));

  if (!process.argv.includes("--send")) {
    console.log("Simulation succeeded; no transaction sent");
    return;
  }

  const signature = await request.rpc({
    commitment: "finalized",
    preflightCommitment: "confirmed",
  });
  console.log(`Signature: ${signature}`);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
