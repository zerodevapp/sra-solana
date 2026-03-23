import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { SraHandler } from "../target/types/sra_handler";

describe("sra-handler", () => {
  anchor.setProvider(anchor.AnchorProvider.env());

  const program = anchor.workspace.sraHandler as Program<SraHandler>;

  it("Is initialized!", async () => {
    const tx = await program.methods.initialize().rpc();
    console.log("Your transaction signature", tx);
  });
});
