import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
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

  it("Exposes the expected generated client instruction surface", async () => {
    const expectedInstructionNames = [
      "acceptAdmin",
      "createRoute",
      "executeActions",
      "finalizeFill",
      "initializeAddressBookRoot",
      "initializeFeeConfig",
      "initializeSra",
      "manageRoute",
      "manageSra",
      "recoverFunds",
      "recoverSourceVault",
      "removeAssetFeeTier",
      "removeProtocolFee",
      "retryActions",
      "setAddressBookEntry",
      "setAssetFeeTier",
      "setFeeRecipient",
      "setPendingAdmin",
      "setProtocolFee",
      "trigger",
    ];
    const removedInstructionNames = [
      "updateAdapters",
      "updateSlippage",
      "validateAndPrepare",
    ];
    const instructionNames = program.idl.instructions.map(({ name }) => name);

    assert.sameMembers(instructionNames, expectedInstructionNames);

    for (const methodName of expectedInstructionNames) {
      assert.isFunction(
        (program.methods as Record<string, unknown>)[methodName]
      );
    }

    for (const removedInstructionName of removedInstructionNames) {
      assert.notInclude(instructionNames, removedInstructionName);
      assert.isUndefined(
        (program.methods as Record<string, unknown>)[removedInstructionName]
      );
    }
  });
});
