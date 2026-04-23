import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { assert } from "chai";
import { SraHandler } from "../target/types/sra_handler";

describe("sra-handler", () => {
  const provider = new anchor.AnchorProvider(
    new anchor.web3.Connection("http://127.0.0.1:8899", "processed"),
    new anchor.Wallet(anchor.web3.Keypair.generate()),
    {}
  );
  anchor.setProvider(provider);

  const program = anchor.workspace.sraHandler as Program<SraHandler>;

  it("Exposes initializeSra in the generated client", async () => {
    assert.isFunction(program.methods.initializeSra);
    assert.equal(program.idl.instructions[0]?.name, "initializeSra");
  });
});
