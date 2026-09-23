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
    cctpAdapter: "0x" + "66".repeat(20),
    cctpAdapterRuntimeHash: "0x" + "77".repeat(32),
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

function encodeCircleMessage(fields: {
  sourceDomain: number;
  destinationDomain: number;
  nonce: Buffer;
  burnToken: Buffer;
  mintRecipient: Buffer;
  amount: bigint;
  maxFee: bigint;
  feeExecuted: bigint;
}) {
  const message = Buffer.alloc(376);
  message.writeUInt32BE(1, 0);
  message.writeUInt32BE(fields.sourceDomain, 4);
  message.writeUInt32BE(fields.destinationDomain, 8);
  fields.nonce.copy(message, 12);
  message.writeUInt32BE(2000, 140);
  message.writeUInt32BE(2000, 144);
  const body = message.subarray(148);
  body.writeUInt32BE(1, 0);
  fields.burnToken.copy(body, 4);
  fields.mintRecipient.copy(body, 36);
  body.writeBigUInt64BE(fields.amount, 92);
  body.writeBigUInt64BE(fields.maxFee, 156);
  body.writeBigUInt64BE(fields.feeExecuted, 188);
  return "0x" + message.toString("hex");
}

test("strict config parsing rejects unknown fields and unsafe amounts", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
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
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: {
          ...sampleConfig.source,
          recipient: "0x01" + "00".repeat(11) + "22".repeat(20),
        },
      }),
    /config\.source\.recipient must be a zero-left-padded EVM address/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: { ...sampleConfig.source, recipient: "0x" + "00".repeat(32) },
      }),
    /config\.source\.recipient must not be the zero address/
  );
  assert.deepEqual(parseCctpConfig(sampleConfig), sampleConfig);
});

test("config rejects fee-bearing or non-finalized CCTP transfers", async () => {
  const { parseCctpConfig } = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: {
          ...sampleConfig.source,
          outputAmount: "999999",
          maxFee: "1",
        },
      }),
    /config\.source\.maxFee must be 0 for Standard Transfer/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...sampleConfig,
        source: { ...sampleConfig.source, minFinalityThreshold: 1000 },
      }),
    /config\.source\.minFinalityThreshold must be 2000 for Standard Transfer/
  );
});

test("state parsing validates resumable stage data", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
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
      arbitrumSraAddress: "0x" + "0d".repeat(20),
      arbitrumSetupAttempts: {
        deployment: [
          {
            hash: "0x" + "0e".repeat(32),
            senderNonce: "4",
            submittedBlock: "100",
          },
        ],
        initiator: [
          {
            hash: "0x" + "0f".repeat(32),
            senderNonce: "5",
            submittedBlock: "101",
          },
        ],
        funding: [
          {
            hash: "0x" + "10".repeat(32),
            senderNonce: "6",
            submittedBlock: "102",
          },
        ],
      },
      arbitrumDepositAttempt: {
        hash: "0x" + "07".repeat(32),
        senderNonce: "7",
        submittedBlock: "103",
      },
      arbitrumDepositAttemptHistory: [
        {
          hash: "0x" + "11".repeat(32),
          senderNonce: "3",
          submittedBlock: "99",
        },
      ],
      arbitrumBaselineDepositNonce: "0",
      arbitrumConfirmedDepositNonce: "1",
      arbitrumBurnMessage: "0x01020304",
      message: "0x0809",
      attestation: "0x0a0b",
      nonce: "0x" + "0c".repeat(32),
      solanaReceiveSignature: "5Nd1mExampleSignature",
      solanaReceiveLastValidBlockHeight: "104",
      solanaFinalizeSignature: "6Nd1mExampleSignature",
      solanaFinalizeLastValidBlockHeight: "105",
      solanaActionsSignature: "7Nd1mExampleSignature",
      solanaRecoverSignature: "8Nd1mExampleSignature",
      arbitrumRecoveryAttemptHistory: [
        {
          hash: "0x" + "12".repeat(32),
          senderNonce: "8",
          submittedBlock: "106",
        },
      ],
      solanaRecoverySignatureHistory: ["8Nd1mExampleSignature"],
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
  assert.throws(
    () =>
      module.parseCctpState({
        ...state,
        destination: {
          ...state.destination,
          evmBurnTransactionHash: "0x" + "13".repeat(32),
        },
      }),
    /state\.destination\.evmBurnTransactionHash must equal state\.destination\.arbitrumDepositAttempt\.hash/
  );
  assert.throws(
    () =>
      module.parseCctpState({
        ...state,
        destination: {
          ...state.destination,
          arbitrumDepositAttempt: {
            ...state.destination.arbitrumDepositAttempt,
            senderNonce: "07",
          },
        },
      }),
    /senderNonce must be a decimal integer/
  );
  assert.throws(
    () =>
      module.parseCctpState({
        ...state,
        destination: {
          ...state.destination,
          arbitrumSetupAttempts: {
            ...state.destination.arbitrumSetupAttempts,
            unexpected: [],
          },
        },
      }),
    /state\.destination\.arbitrumSetupAttempts has unknown field unexpected/
  );
});

test("reverse state updates preserve the successful source object", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  const source = {
    setupSignature: "3Nd1mExampleSignature",
    solanaSignature: "4Nd1mExampleSignature",
    circleEventAccount: "11111111111111111111111111111111",
    message: "0x0102",
    attestation: "0x0304",
    nonce: "0x" + "05".repeat(32),
    evmTransactionHash: "0x" + "06".repeat(32),
  };
  const state = {
    version: 1 as const,
    source,
    destination: {
      evmBurnTransactionHash: "0x" + "07".repeat(32),
      arbitrumDepositAttempt: {
        hash: "0x" + "07".repeat(32),
        senderNonce: "1",
        submittedBlock: "100",
      },
    },
  };
  const response = {
    messages: [
      {
        status: "complete",
        message: "0x0809",
        attestation: "0x0a0b",
        decodedMessage: {
          sourceDomain: 3,
          destinationDomain: 5,
          nonce: "0x" + "0c".repeat(32),
          decodedMessageBody: {
            mintRecipient: "0x" + "0d".repeat(32),
            amount: "1000000",
          },
        },
      },
    ],
  };
  const snapshot = structuredClone(source);

  const updated = module.mergeIrisAttestation(state, "destination", response, {
    sourceDomain: 3,
    destinationDomain: 5,
    mintRecipient: "0x" + "0d".repeat(32),
    amount: "1000000",
  });

  assert.deepEqual(state.source, snapshot);
  assert.deepEqual(updated.source, snapshot);
  assert.deepEqual(updated.destination, {
    ...state.destination,
    message: "0x0809",
    attestation: "0x0a0b",
    nonce: "0x" + "0c".repeat(32),
  });
});

test("Iris V2 normalizes its live response and preserves recorded progress", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.mergeIrisAttestation, "function");
  const evmRecipient = "0x" + "44".repeat(20);
  const mintRecipient = "0x" + "00".repeat(12) + evmRecipient.slice(2);
  const response = {
    messages: [
      {
        status: "complete",
        message: "0x0102",
        attestation: "0x0304",
        decodedMessage: {
          sourceDomain: "5",
          destinationDomain: "3",
          nonce: "0x" + "05".repeat(32),
          decodedMessageBody: {
            mintRecipient: evmRecipient,
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

test("persisted Circle message rejects a stale configured recipient", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.validatePersistedCircleMessage, "function");
  const nonce = Buffer.alloc(32, 1);
  const burnToken = Buffer.alloc(32, 2);
  const oldRecipient = Buffer.alloc(32, 3);
  const currentRecipient = Buffer.alloc(32, 4);
  const message = encodeCircleMessage({
    sourceDomain: 3,
    destinationDomain: 5,
    nonce,
    burnToken,
    mintRecipient: oldRecipient,
    amount: 1_000_000n,
    maxFee: 0n,
    feeExecuted: 0n,
  });

  assert.throws(
    () =>
      module.validatePersistedCircleMessage(message, {
        sourceDomain: 3,
        destinationDomain: 5,
        nonce: "0x" + nonce.toString("hex"),
        burnToken: "0x" + burnToken.toString("hex"),
        mintRecipient: "0x" + currentRecipient.toString("hex"),
        amount: 1_000_000n,
        maxFee: 0n,
        feeExecuted: 0n,
        minFinalityThreshold: 2000,
        finalityThresholdExecuted: 2000,
      }),
    /persisted Circle mint recipient does not match current config/
  );
});

test("native v1 compilation enforces explicit config and the repository envelope", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
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

test("oversized native v1 settlement selects the split fallback", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.selectDestinationSettlementMode, "function");
  const payer = web3.Keypair.generate();
  const oversized = new web3.TransactionInstruction({
    programId: web3.Keypair.generate().publicKey,
    keys: [],
    data: Buffer.alloc(5_000, 1),
  });
  const latestBlockhash = {
    blockhash: "11111111111111111111111111111111" as Blockhash,
    lastValidBlockHeight: 1n,
  };

  assert.equal(
    await module.selectDestinationSettlementMode(
      [oversized],
      [payer.secretKey],
      latestBlockhash
    ),
    "fallback"
  );
});

test("state-changing commands simulate by default and verify after send", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
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

test("post-state account reads retry transient RPC lag", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.retryPostStateRead, "function");
  const account = { owner: "handler" };
  let reads = 0;
  const slots: (number | undefined)[] = [];

  const result = await module.retryPostStateRead(
    async (minContextSlot) => {
      slots.push(minContextSlot);
      reads += 1;
      if (reads === 1) throw new Error("minimum context slot not reached");
      return reads === 2 ? null : account;
    },
    123_456,
    3,
    0
  );

  assert.equal(result, account);
  assert.equal(reads, 3);
  assert.deepEqual(slots, [123_456, 123_456, 123_456]);
});

test("program-owned verification reads from the finalized confirmation slot", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.verifyProgramOwnedAccount, "function");
  const account = web3.Keypair.generate().publicKey;
  const owner = web3.Keypair.generate().publicKey;
  const configs: unknown[] = [];
  let reads = 0;
  const connection = {
    getAccountInfo: async (_account: web3.PublicKey, config: unknown) => {
      configs.push(config);
      reads += 1;
      if (reads === 1) throw new Error("minimum context slot not reached");
      if (reads === 2) return null;
      return { owner, data: Buffer.alloc(72) };
    },
  } as unknown as web3.Connection;

  const info = await module.verifyProgramOwnedAccount(
    connection,
    account,
    owner,
    "test account",
    123_456
  );

  assert(info.owner.equals(owner));
  assert.deepEqual(configs, [
    { commitment: "finalized", minContextSlot: 123_456 },
    { commitment: "finalized", minContextSlot: 123_456 },
    { commitment: "finalized", minContextSlot: 123_456 },
  ]);
});

test("destination fallback dry-run reports the state-dependent finalize stage", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.executeDestinationSettlementWorkflow, "function");
  const events: string[] = [];
  const stage = (name: string) => ({
    simulate: async () => events.push(`${name}:simulate`),
    broadcast: async () => {
      events.push(`${name}:broadcast`);
      return `${name}-signature`;
    },
    writeArtifact: async () => events.push(`${name}:artifact`),
    confirm: async () => events.push(`${name}:confirm`),
    verifyPostState: async () => events.push(`${name}:verify`),
  });

  const result = await module.executeDestinationSettlementWorkflow(
    { send: false, combinedFits: false, alreadyReceived: false },
    {
      combined: stage("combined"),
      receive: stage("receive"),
      prepareFinalize: async () => {
        events.push("finalize:prepare");
        return stage("finalize");
      },
      reportDeferredFinalize: () => events.push("finalize:deferred"),
    }
  );

  assert.deepEqual(events, ["receive:simulate", "finalize:deferred"]);
  assert.deepEqual(result, { mode: "simulation", finalizeDeferred: true });
});

test("destination fallback prepares finalize only after receive confirmation and verification", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  const events: string[] = [];
  const stage = (name: string) => ({
    simulate: async () => events.push(`${name}:simulate`),
    broadcast: async () => {
      events.push(`${name}:broadcast`);
      return `${name}-signature`;
    },
    writeArtifact: async (id: string) => events.push(`${name}:artifact:${id}`),
    confirm: async (id: string) => events.push(`${name}:confirm:${id}`),
    verifyPostState: async () => events.push(`${name}:verify`),
  });

  const result = await module.executeDestinationSettlementWorkflow(
    { send: true, combinedFits: false, alreadyReceived: false },
    {
      combined: stage("combined"),
      receive: stage("receive"),
      prepareFinalize: async () => {
        events.push("finalize:prepare");
        return stage("finalize");
      },
      reportDeferredFinalize: () => events.push("finalize:deferred"),
    }
  );

  assert.deepEqual(events, [
    "receive:simulate",
    "receive:broadcast",
    "receive:artifact:receive-signature",
    "receive:confirm:receive-signature",
    "receive:verify",
    "finalize:prepare",
    "finalize:simulate",
    "finalize:broadcast",
    "finalize:artifact:finalize-signature",
    "finalize:confirm:finalize-signature",
    "finalize:verify",
  ]);
  assert.deepEqual(result, {
    mode: "sent",
    receiveId: "receive-signature",
    finalizeId: "finalize-signature",
  });
});

test("destination resumption retries failed finalization without reminting", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.planDestinationResumption, "function");

  assert.equal(
    module.planDestinationResumption({
      usedNonce: true,
      receiptFinalized: false,
      receiveAttempt: "confirmed",
      finalizeAttempt: "failed",
    }),
    "finalize-only"
  );
  assert.equal(
    module.planDestinationResumption({
      usedNonce: true,
      receiptFinalized: false,
      receiveAttempt: "confirmed",
      finalizeAttempt: "expired",
    }),
    "finalize-only"
  );
  assert.throws(
    () =>
      module.planDestinationResumption({
        usedNonce: true,
        receiptFinalized: false,
        receiveAttempt: "confirmed",
        finalizeAttempt: "pending",
      }),
    /destination finalization attempt is still pending/
  );
  assert.throws(
    () =>
      module.planDestinationResumption({
        usedNonce: true,
        receiptFinalized: false,
        receiveAttempt: "confirmed",
        finalizeAttempt: "confirmed",
      }),
    /confirmed finalization attempt has no fill receipt post-state/
  );
});

test("EVM boundary verification rejects mismatched chain and cast signer", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e-utils");
  assert.equal(typeof module.verifyEvmBoundary, "function");
  const canary = "0x4444444444444444444444444444444444444444";
  const verified = await module.verifyEvmBoundary(
    {
      expectedChainId: 42161n,
      expectedSigner: canary,
    },
    {
      readChainId: async () => 42161n,
      resolveSigner: async () => canary.toUpperCase(),
    }
  );

  assert.deepEqual(verified, { chainId: 42161n, signer: canary });
  await assert.rejects(
    () =>
      module.verifyEvmBoundary(
        { expectedChainId: 42161n, expectedSigner: canary },
        {
          readChainId: async () => 1n,
          resolveSigner: async () => canary,
        }
      ),
    /EVM RPC chain id 1 does not match expected 42161/
  );
  await assert.rejects(
    () =>
      module.verifyEvmBoundary(
        { expectedChainId: 42161n, expectedSigner: canary },
        {
          readChainId: async () => 42161n,
          resolveSigner: async () =>
            "0x5555555555555555555555555555555555555555",
        }
      ),
    /cast account resolves to 0x5555555555555555555555555555555555555555, expected 0x4444444444444444444444444444444444444444/
  );
});

test("destination route post-state pins the 100 percent transfer recipient", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.validateDestinationTransferAction, "function");
  const recipient = web3.Keypair.generate().publicKey;
  const other = web3.Keypair.generate().publicKey;

  assert.doesNotThrow(() =>
    module.validateDestinationTransferAction(
      [{ splTransfer: { recipient, amountBps: 10_000 } }],
      recipient
    )
  );
  assert.throws(
    () =>
      module.validateDestinationTransferAction(
        [{ splTransfer: { recipient: other, amountBps: 10_000 } }],
        recipient
      ),
    /destination transfer recipient does not match config/
  );
  assert.throws(
    () =>
      module.validateDestinationTransferAction(
        [{ splTransfer: { recipient, amountBps: 9_999 } }],
        recipient
      ),
    /destination transfer amountBps must be 10000/
  );
});

test("destination settlement post-state enforces amount and vault conservation", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.equal(typeof module.validateSettlementPostState, "function");
  const complete = {
    amount: 100n,
    receivedInThisCommand: true,
    preRouteBalance: 7n,
    postRouteBalance: 7n,
    preExecutionBalance: 3n,
    postExecutionBalance: 103n,
    receiptAmount: 100n,
  };

  assert.doesNotThrow(() => module.validateSettlementPostState(complete));
  assert.doesNotThrow(() =>
    module.validateSettlementPostState({
      ...complete,
      receivedInThisCommand: false,
      preRouteBalance: 107n,
    })
  );
  assert.throws(
    () =>
      module.validateSettlementPostState({
        ...complete,
        postExecutionBalance: 102n,
      }),
    /execution vault balance does not include the settled amount/
  );
  assert.throws(
    () =>
      module.validateSettlementPostState({
        ...complete,
        postRouteBalance: 8n,
      }),
    /route vault conservation check failed/
  );
  assert.throws(
    () =>
      module.validateSettlementPostState({
        ...complete,
        receiptAmount: 99n,
      }),
    /fill receipt amount does not match the settled amount/
  );
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

test("Arbitrum sends require explicit isolated keystore credentials", async () => {
  const module = await import("../scripts/cctp-mainnet-e2e");
  assert.throws(
    () => module.parseCliArguments(["arbitrum-receive", "--send"]),
    /arbitrum-receive --send requires --keystore and --password-file/
  );

  const parsed = module.parseCliArguments([
    "arbitrum-receive",
    "--send",
    "--keystore",
    "/keys/cctp-canary",
    "--password-file",
    "/tmp/cctp-password",
  ]);
  assert.equal(parsed.keystorePath, "/keys/cctp-canary");
  assert.equal(parsed.passwordFile, "/tmp/cctp-password");
  assert.deepEqual(module.castWalletArguments("cctp-canary", parsed), [
    "--keystore",
    "/keys/cctp-canary",
    "--password-file",
    "/tmp/cctp-password",
  ]);
  assert.doesNotThrow(() =>
    module.parseCliArguments([
      "preflight",
      "--keystore",
      "/keys/cctp-canary",
      "--password-file",
      "/tmp/cctp-password",
    ])
  );
  assert.throws(
    () =>
      module.parseCliArguments([
        "source-deposit",
        "--keystore",
        "/keys/cctp-canary",
        "--password-file",
        "/tmp/cctp-password",
      ]),
    /wallet flags are only valid for preflight and arbitrum-receive/
  );
  assert.deepEqual(
    module.sanitizeCastEnvironment({
      PATH: "/bin",
      ETH_PASSWORD: "/tmp/cctp-password",
      ETH_KEYSTORE: "/keys/cctp-canary",
      ETH_KEYSTORE_ACCOUNT: "cctp-canary",
    }),
    { PATH: "/bin" }
  );
});
