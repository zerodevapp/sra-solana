import assert from "node:assert/strict";
import test from "node:test";

const validConfig = {
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
    cctpAdapterRuntimeHash:
      "0x07ad118d6cc8642c86c03827f276d8b791a65e5c99a3845faf186be720a1455d",
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

test("Arbitrum adapter config accepts only the confirmed deployment fields", async () => {
  const { parseCctpConfig } = await import("../scripts/cctp-mainnet-e2e-utils");

  assert.deepEqual(parseCctpConfig(validConfig), validConfig);
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        arbitrum: {
          ...validConfig.arbitrum,
          cctpAdapter: "0x" + "00".repeat(20),
        },
      }),
    /config\.arbitrum\.cctpAdapter must not be the zero address/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        arbitrum: { ...validConfig.arbitrum, cctpAdapter: "0x1234" },
      }),
    /config\.arbitrum\.cctpAdapter must be 0x-prefixed 20-byte hex/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        arbitrum: {
          ...validConfig.arbitrum,
          cctpAdapterRuntimeHash: "0x1234",
        },
      }),
    /config\.arbitrum\.cctpAdapterRuntimeHash must be 0x-prefixed 32-byte hex/
  );

  for (const field of [
    "handler",
    "factory",
    "implementation",
    "addressBook",
    "resolver",
    "tokenMessenger",
    "solanaUsdcMarker",
  ]) {
    assert.throws(
      () =>
        parseCctpConfig({
          ...validConfig,
          arbitrum: { ...validConfig.arbitrum, [field]: "0x01" },
        }),
      new RegExp(`config\\.arbitrum has unknown field ${field}`)
    );
  }
});

test("Arbitrum adapter config pins Circle contracts and canary economics", async () => {
  const { parseCctpConfig } = await import("../scripts/cctp-mainnet-e2e-utils");

  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        arbitrum: {
          ...validConfig.arbitrum,
          messageTransmitter: "0x" + "88".repeat(20),
        },
      }),
    /config\.arbitrum\.messageTransmitter must be/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: {
          ...validConfig.source,
          targetToken: "0x" + "88".repeat(32),
        },
      }),
    /config\.source\.targetToken must be/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        destination: {
          ...validConfig.destination,
          remoteUsdc: "0x" + "88".repeat(32),
        },
      }),
    /config\.destination\.remoteUsdc must be/
  );

  for (const amount of ["1", "999999", "1000001"]) {
    assert.throws(
      () =>
        parseCctpConfig({
          ...validConfig,
          destination: { ...validConfig.destination, amount },
        }),
      /config\.destination\.amount must be 1000000/
    );
  }
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, inputAmount: "999999" },
      }),
    /config\.source\.inputAmount must be 1000000/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, outputAmount: "999999" },
      }),
    /config\.source\.outputAmount must be 1000000/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, protocolFeeAmount: "1" },
      }),
    /config\.source\.protocolFeeAmount must be 0/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, allowedSlippageBps: 1 },
      }),
    /config\.source\.allowedSlippageBps must be 0/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, maxFee: "1" },
      }),
    /config\.source\.maxFee must be 0 for Standard Transfer/
  );
  assert.throws(
    () =>
      parseCctpConfig({
        ...validConfig,
        source: { ...validConfig.source, minFinalityThreshold: 1000 },
      }),
    /config\.source\.minFinalityThreshold must be 2000 for Standard Transfer/
  );
});

test("Arbitrum CCTP adapter verification binds code and constructor state", async () => {
  const { validateArbitrumCctpAdapter: validate } = await import(
    "../scripts/cctp-mainnet-e2e-evm"
  );
  const observation = {
    chainId: 42161n,
    address: validConfig.arbitrum.cctpAdapter,
    code: "0x6000",
    handler: "0x09bE9c9dd3219e81cA2F41AEF85481f86595f592",
    tokenMessenger: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d",
    messageTransmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64",
    usdc: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
    owner: "0x9775137314fE595c943712B0b336327dfa80aE8A",
    localDomain: 3n,
    solanaDomain: 5n,
  };

  assert.doesNotThrow(() => validate(validConfig.arbitrum, observation));
  assert.throws(
    () => validate(validConfig.arbitrum, { ...observation, code: "0x6001" }),
    /runtime hash/
  );
  for (const [field, value, message] of [
    ["chainId", 1n, /chain id/],
    ["address", "0x" + "88".repeat(20), /address/],
    ["code", "0x", /bytecode/],
    ["handler", "0x" + "88".repeat(20), /handler/],
    ["tokenMessenger", "0x" + "88".repeat(20), /TokenMessenger/],
    ["messageTransmitter", "0x" + "88".repeat(20), /MessageTransmitter/],
    ["usdc", "0x" + "88".repeat(20), /USDC/],
    ["owner", "0x" + "88".repeat(20), /owner/],
    ["localDomain", 4n, /local domain/],
    ["solanaDomain", 6n, /Solana domain/],
  ] as const) {
    assert.throws(
      () => validate(validConfig.arbitrum, { ...observation, [field]: value }),
      message
    );
  }
});
