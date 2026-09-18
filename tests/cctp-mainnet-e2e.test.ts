import assert from "node:assert/strict";
import test from "node:test";
import { web3 } from "@anchor-lang/core";
import type { Blockhash } from "@solana/kit";

const sampleConfig = {
  rpcUrl: "https://api.mainnet-beta.solana.com",
  payerKeypair: "/keys/payer.json",
  source: {
    ownerKeypair: "/keys/source-owner.json",
    initiatorKeypair: "/keys/initiator.json",
    addressBookRootKeypair: "/keys/address-book-root.json",
    salt: "0x" + "11".repeat(32),
    recipient: "0x" + "00".repeat(12) + "22".repeat(20),
    targetToken:
      "0x000000000000000000000000af88d065e77c8cC2239327C5EDb3A432268e5831",
    targetChainId: 42161,
    targetDecimals: 6,
    allowedSlippageBps: 0,
    inputAmount: "1000000",
    outputAmount: "1000000",
    protocolFeeAmount: "0",
    maxFee: "0",
    minFinalityThreshold: 2000,
  },
  arbitrum: {
    rpcUrl: "https://arb.example",
    account: "cctp-canary",
    canaryAddress: "0x" + "44".repeat(20),
    messageTransmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64",
  },
  destination: {
    ownerKeypair: "/keys/destination-owner.json",
    salt: "0x" + "55".repeat(32),
    sourceChainId: 42161,
    remoteUsdc:
      "0x" + "00".repeat(12) + "af88d065e77c8cC2239327C5EDb3A432268e5831",
    recipient: "11111111111111111111111111111111",
    executor: "11111111111111111111111111111111",
    amount: "1000000",
  },
};

test("strict config parsing rejects unknown fields and unsafe amounts", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.parseCctpConfig, "function");

  const parseCctpConfig = module.parseCctpConfig as (value: unknown) => unknown;
  assert.throws(
    () => parseCctpConfig({ ...sampleConfig, unexpected: true }),
    /config has unknown field unexpected/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: { ...sampleConfig.source, inputAmount: "0" },
      }),
    /config\.source\.inputAmount must be a positive decimal integer/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: { ...sampleConfig.source, targetChainId: 1 },
      }),
    /config\.source\.targetChainId must be 42161/
  );
  assert.deepEqual(parseCctpConfig(sampleConfig), sampleConfig);
});

test("state parsing validates resumable stage data", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.parseCctpState, "function");

  const state = {
    version: 1,
    configPath: "/tmp/cctp-mainnet.json",
    source: {
      solanaSignature: "4Nd1mExampleSignature",
      circleEventAccount: "11111111111111111111111111111111",
      message: "0x0102",
      attestation: "0x0304",
      nonce: "0x" + "05".repeat(32),
      evmTransactionHash: "0x" + "06".repeat(32),
    },
    destination: {
      evmBurnTransactionHash: "0x" + "07".repeat(32),
      message: "0x0809",
      attestation: "0x0a0b",
      nonce: "0x" + "0c".repeat(32),
      solanaReceiveSignature: "5Nd1mExampleSignature",
      solanaFinalizeSignature: "6Nd1mExampleSignature",
      solanaActionsSignature: "7Nd1mExampleSignature",
      solanaRecoverSignature: "8Nd1mExampleSignature",
    },
  };
  assert.deepEqual(module.parseCctpState(state), state);
  assert.throws(
    () =>
      module.parseCctpState({
        ...state,
        destination: { ...state.destination, burnAgain: true },
      }),
    /state\.destination has unknown field burnAgain/
  );
  assert.throws(
    () =>
      module.parseCctpState({
        ...state,
        source: { ...state.source, message: "0102" },
      }),
    /state\.source\.message must be 0x-prefixed even-length hex/
  );
});

test("Iris resumption validates the route and preserves recorded progress", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.mergeIrisAttestation, "function");
  const mintRecipient = "0x" + "00".repeat(12) + "44".repeat(20);
  const response = {
    messages: [
      {
        status: "complete",
        message: "0x0102",
        attestation: "0x0304",
        decodedMessage: {
          sourceDomain: 5,
          destinationDomain: 3,
          nonce: "0x" + "05".repeat(32),
          decodedMessageBody: {
            mintRecipient,
            amount: "1000000",
          },
        },
      },
    ],
  };
  const state = {
    version: 1 as const,
    source: {
      solanaSignature: "4Nd1mExampleSignature",
      circleEventAccount: "11111111111111111111111111111111",
    },
  };

  assert.deepEqual(
    module.mergeIrisAttestation(state, "source", response, {
      sourceDomain: 5,
      destinationDomain: 3,
      mintRecipient,
      amount: "1000000",
    }),
    {
      ...state,
      source: {
        ...state.source,
        message: "0x0102",
        attestation: "0x0304",
        nonce: "0x" + "05".repeat(32),
      },
    }
  );
  assert.throws(
    () =>
      module.mergeIrisAttestation(state, "source", response, {
        sourceDomain: 5,
        destinationDomain: 3,
        mintRecipient,
        amount: "999999",
      }),
    /Iris amount 1000000 does not match expected 999999/
  );
});

test("native v1 compilation enforces explicit config and the repository envelope", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.compileNativeV1Transaction, "function");
  const payer = web3.Keypair.generate();
  const transfer = web3.SystemProgram.transfer({
    fromPubkey: payer.publicKey,
    toPubkey: web3.Keypair.generate().publicKey,
    lamports: 1,
  });
  const compiled = await module.compileNativeV1Transaction(
    [transfer],
    [payer.secretKey],
    {
      blockhash: "11111111111111111111111111111111" as Blockhash,
      lastValidBlockHeight: 1n,
    }
  );

  assert.equal(compiled.transaction.messageBytes[0], 0x81);
  assert.equal(compiled.addressCount, 3);
  assert(compiled.size <= 4096);
  assert.deepEqual(compiled.config, {
    computeUnitLimit: 400_000,
    loadedAccountsDataSizeLimit: 67_108_864,
  });

  const tooManyAccounts = new web3.TransactionInstruction({
    programId: web3.Keypair.generate().publicKey,
    keys: Array.from({ length: 64 }, () => ({
      pubkey: web3.Keypair.generate().publicKey,
      isSigner: false,
      isWritable: false,
    })),
  });
  await assert.rejects(
    () =>
      module.compileNativeV1Transaction([tooManyAccounts], [payer.secretKey], {
        blockhash: "11111111111111111111111111111111" as Blockhash,
        lastValidBlockHeight: 1n,
      }),
    /native v1 transaction uses 66 inline addresses; maximum is 64/
  );
});

test("state-changing commands simulate by default and verify after send", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.executeWithSendGate, "function");
  const dryRunEvents: string[] = [];
  const dryRun = await module.executeWithSendGate(false, {
    simulate: async () => dryRunEvents.push("simulate"),
    send: async () => {
      dryRunEvents.push("send");
      return "signature";
    },
    verify: async () => dryRunEvents.push("verify"),
  });

  assert.deepEqual(dryRunEvents, ["simulate"]);
  assert.deepEqual(dryRun, { mode: "simulation" });

  const sendEvents: string[] = [];
  const sent = await module.executeWithSendGate(true, {
    simulate: async () => sendEvents.push("simulate"),
    send: async () => {
      sendEvents.push("send");
      return "signature";
    },
    verify: async (signature: string) => sendEvents.push(`verify:${signature}`),
  });

  assert.deepEqual(sendEvents, ["simulate", "send", "verify:signature"]);
  assert.deepEqual(sent, { mode: "sent", id: "signature" });
});

test("CLI parsing exposes only the nine CCTP operator commands", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.deepEqual(module.CCTP_COMMANDS, [
    "preflight",
    "source-setup",
    "source-deposit",
    "iris-wait",
    "arbitrum-receive",
    "destination-setup",
    "destination-receive-finalize",
    "destination-actions",
    "destination-recover",
  ]);
  assert.deepEqual(
    module.parseCliArguments([
      "source-deposit",
      "--config",
      "cctp-mainnet.json",
      "--state",
      "cctp-state.json",
    ]),
    {
      command: "source-deposit",
      configPath: "cctp-mainnet.json",
      statePath: "cctp-state.json",
      send: false,
      direction: undefined,
      retry: false,
    }
  );
  assert.equal(
    module.parseCliArguments([
      "destination-actions",
      "--state",
      "cctp-state.json",
      "--send",
    ]).send,
    true
  );
  assert.throws(
    () => module.parseCliArguments(["source-deposit", "--broadcast"]),
    /unknown flag --broadcast/
  );
});
