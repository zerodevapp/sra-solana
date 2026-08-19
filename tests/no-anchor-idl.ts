import * as anchor from "@coral-xyz/anchor";
import { assert } from "chai";

describe("production program artifacts", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const idlInstructionTag = Buffer.from([
    0x40, 0xf4, 0xbc, 0x78, 0xa7, 0xe9, 0x69, 0x0a,
  ]);

  async function assertIdlInstructionsDisabled(
    programId: anchor.web3.PublicKey
  ) {
    const transaction = new anchor.web3.Transaction().add(
      new anchor.web3.TransactionInstruction({
        keys: [],
        programId,
        data: idlInstructionTag,
      })
    );
    transaction.feePayer = provider.wallet.publicKey;
    transaction.recentBlockhash = (
      await provider.connection.getLatestBlockhash()
    ).blockhash;

    const signedTransaction = await provider.wallet.signTransaction(
      transaction
    );
    const simulation = await provider.connection.simulateTransaction(
      signedTransaction
    );

    assert.deepEqual(simulation.value.err, {
      InstructionError: [0, { Custom: 1000 }],
    });
  }

  it("rejects Anchor IDL instructions in the handler artifact", async () => {
    await assertIdlInstructionsDisabled(anchor.workspace.sraHandler.programId);
  });

  it("rejects Anchor IDL instructions in the adapter artifact", async () => {
    await assertIdlInstructionsDisabled(
      anchor.workspace.sraAcrossAdapter.programId
    );
  });
});
