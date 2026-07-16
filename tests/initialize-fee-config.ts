import { web3 } from "@coral-xyz/anchor";
import { assert } from "chai";
import {
  EXPECTED_FEE_CONFIG,
  HANDLER_PROGRAM_ID,
  MAINNET_GENESIS_HASH,
  assertSendAcknowledged,
  deriveFeeConfig,
  parseShouldSend,
  simulateThenMaybeSend,
  validateCreatedFeeConfig,
  validatePreflightState,
  validateStaticTargets,
} from "../scripts/initialize-fee-config";

function testTransaction(payer: web3.Keypair): web3.Transaction {
  return new web3.Transaction({
    feePayer: payer.publicKey,
    recentBlockhash: web3.PublicKey.default.toBase58(),
  }).add(
    web3.SystemProgram.transfer({
      fromPubkey: payer.publicKey,
      toPubkey: payer.publicKey,
      lamports: 0,
    })
  );
}

describe("initialize-fee-config operator", () => {
  it("pins the deployed mainnet handler and fee-config PDA", () => {
    assert.equal(
      HANDLER_PROGRAM_ID.toBase58(),
      "2U8tvBcQjZx8r6RG4r1pnxdG9LfJF4Ya4hZsNGRmhqt9"
    );
    assert.equal(
      EXPECTED_FEE_CONFIG.toBase58(),
      "8EsP57fThwiKcGKzb3YeVGRRMZwW7TR1bQ8gAnNkNjLA"
    );
    assert.equal(
      MAINNET_GENESIS_HASH,
      "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"
    );
    assert.equal(
      deriveFeeConfig(HANDLER_PROGRAM_ID).toBase58(),
      EXPECTED_FEE_CONFIG.toBase58()
    );
  });

  it("rejects a mismatched IDL program address", () => {
    assert.throws(
      () => validateStaticTargets(web3.PublicKey.default, EXPECTED_FEE_CONFIG),
      "IDL program address"
    );
  });

  it("rejects a mismatched derived fee-config PDA", () => {
    assert.throws(
      () => validateStaticTargets(HANDLER_PROGRAM_ID, web3.PublicKey.default),
      "fee-config PDA"
    );
  });

  it("requires an explicit send flag and first-caller acknowledgement", () => {
    assert.isFalse(parseShouldSend([]));
    assert.isTrue(parseShouldSend(["--send"]));
    assert.throws(() => parseShouldSend(["--other"]), "Unknown argument");
    assert.doesNotThrow(() => assertSendAcknowledged(false, undefined));
    assert.throws(
      () => assertSendAcknowledged(true, undefined),
      "FEE_CONFIG_INIT_ACK"
    );
    assert.doesNotThrow(() =>
      assertSendAcknowledged(true, "I_UNDERSTAND_FIRST_CALLER_WINS")
    );
  });

  it("simulates without signing or sending by default", async () => {
    const payer = web3.Keypair.generate();
    const transaction = testTransaction(payer);
    let sends = 0;
    const result = await simulateThenMaybeSend(transaction, payer, false, {
      simulate: async () => ({ err: null, logs: ["simulated"] }),
      send: async () => {
        sends += 1;
        return "signature";
      },
      confirm: async () => undefined,
    });

    assert.deepEqual(result, { logs: ["simulated"], signature: null });
    assert.equal(sends, 0);
    assert.isFalse(
      transaction.signatures.some(({ signature }) => signature !== null)
    );
  });

  it("does not send when simulation fails", async () => {
    const payer = web3.Keypair.generate();
    let sends = 0;

    try {
      await simulateThenMaybeSend(testTransaction(payer), payer, true, {
        simulate: async () => ({ err: "failed", logs: ["failure"] }),
        send: async () => {
          sends += 1;
          return "signature";
        },
        confirm: async () => undefined,
      });
      assert.fail("simulation failure must reject");
    } catch (error) {
      assert.include(String(error), "Simulation failed");
    }
    assert.equal(sends, 0);
  });

  it("signs, sends, and confirms only after successful simulation", async () => {
    const payer = web3.Keypair.generate();
    const transaction = testTransaction(payer);
    let confirmed: string | null = null;

    const result = await simulateThenMaybeSend(transaction, payer, true, {
      simulate: async () => ({ err: null, logs: [] }),
      send: async (raw) => {
        assert.isAbove(raw.length, 0);
        return "signature";
      },
      confirm: async (signature) => {
        confirmed = signature;
      },
    });

    assert.equal(result.signature, "signature");
    assert.equal(confirmed, "signature");
    assert.isTrue(
      transaction.signatures.some(({ signature }) => signature !== null)
    );
  });

  it("fails closed on the wrong cluster, program, or existing singleton", () => {
    assert.throws(
      () => validatePreflightState("devnet", true, false),
      "not Solana mainnet"
    );
    assert.throws(
      () => validatePreflightState(MAINNET_GENESIS_HASH, false, false),
      "not executable"
    );
    assert.throws(
      () => validatePreflightState(MAINNET_GENESIS_HASH, true, true),
      "already exists"
    );
    assert.doesNotThrow(() =>
      validatePreflightState(MAINNET_GENESIS_HASH, true, false)
    );
  });

  it("rejects unexpected finalized fee-config state", () => {
    const admin = web3.Keypair.generate().publicKey;
    const validState = {
      admin,
      pendingAdmin: web3.PublicKey.default,
      feeRecipient: web3.PublicKey.default,
    };

    assert.throws(
      () => validateCreatedFeeConfig(web3.PublicKey.default, validState, admin),
      "wrong owner"
    );
    assert.throws(
      () =>
        validateCreatedFeeConfig(
          HANDLER_PROGRAM_ID,
          { ...validState, admin: web3.Keypair.generate().publicKey },
          admin
        ),
      "Unexpected fee admin"
    );
    assert.throws(
      () =>
        validateCreatedFeeConfig(
          HANDLER_PROGRAM_ID,
          {
            ...validState,
            feeRecipient: web3.Keypair.generate().publicKey,
          },
          admin
        ),
      "Unexpected fee recipient"
    );
    assert.doesNotThrow(() =>
      validateCreatedFeeConfig(HANDLER_PROGRAM_ID, validState, admin)
    );
  });

  it("propagates finalized confirmation failure", async () => {
    const payer = web3.Keypair.generate();

    try {
      await simulateThenMaybeSend(testTransaction(payer), payer, true, {
        simulate: async () => ({ err: null, logs: [] }),
        send: async () => "signature",
        confirm: async () => {
          throw new Error("confirmation failed");
        },
      });
      assert.fail("confirmation failure must reject");
    } catch (error) {
      assert.include(String(error), "confirmation failed");
    }
  });
});
