import { web3 } from "@anchor-lang/core";
import {
  AccountRole,
  addSignersToTransactionMessage,
  address,
  appendTransactionMessageInstructions,
  assertIsTransactionWithinSizeLimit,
  compileTransactionMessage,
  createKeyPairSignerFromBytes,
  createTransactionMessage,
  getTransactionSize,
  pipe,
  setTransactionMessageConfig,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Blockhash,
  type Instruction,
  type Transaction,
  type V1TransactionConfig,
} from "@solana/kit";
import {
  ARBITRUM_CCTP_DEPLOYMENT,
  parseEvmDestinationState,
  type EvmDestinationState,
} from "./cctp-mainnet-e2e-evm";

export const ARBITRUM_CHAIN_ID = 42161;
export const ARBITRUM_USDC = `0x${"0".repeat(
  24
)}${ARBITRUM_CCTP_DEPLOYMENT.usdc.slice(2)}`;
export const EVM_MESSAGE_TRANSMITTER =
  ARBITRUM_CCTP_DEPLOYMENT.messageTransmitter;
export const CCTP_CANARY_AMOUNT = "1000000";

export const NATIVE_V1_CONFIG: Readonly<V1TransactionConfig> = {
  computeUnitLimit: 400_000,
  loadedAccountsDataSizeLimit: 67_108_864,
};

export type V1LatestBlockhash = Readonly<{
  blockhash: Blockhash;
  lastValidBlockHeight: bigint;
}>;

export type CctpConfig = Readonly<{
  rpcUrl: string;
  payerKeypair: string;
  source: Readonly<{
    ownerKeypair: string;
    initiatorKeypair: string;
    addressBookRootKeypair: string;
    salt: string;
    recipient: string;
    targetToken: string;
    targetChainId: number;
    targetDecimals: number;
    allowedSlippageBps: number;
    inputAmount: string;
    outputAmount: string;
    protocolFeeAmount: string;
    maxFee: string;
    minFinalityThreshold: number;
  }>;
  arbitrum: Readonly<{
    rpcUrl: string;
    account: string;
    canaryAddress: string;
    messageTransmitter: string;
    cctpAdapter: string;
    cctpAdapterRuntimeHash: string;
  }>;
  destination: Readonly<{
    ownerKeypair: string;
    salt: string;
    sourceChainId: number;
    remoteUsdc: string;
    recipient: string;
    executor: string;
    amount: string;
  }>;
}>;

export type CctpState = Readonly<{
  version: 1;
  configPath?: string;
  source?: Readonly<{
    setupSignature?: string;
    solanaSignature?: string;
    circleEventAccount?: string;
    message?: string;
    attestation?: string;
    nonce?: string;
    evmTransactionHash?: string;
  }>;
  destination?: EvmDestinationState;
}>;

export type IrisExpectation = Readonly<{
  sourceDomain: number;
  destinationDomain: number;
  mintRecipient: string;
  amount: string;
}>;

export type JsonObject = Record<string, unknown>;

export function objectAt(value: unknown, path: string): JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  return value as JsonObject;
}

function exactFields(
  value: JsonObject,
  fields: readonly string[],
  path: string
) {
  for (const key of Object.keys(value)) {
    if (!fields.includes(key))
      throw new Error(`${path} has unknown field ${key}`);
  }
  for (const key of fields) {
    if (!(key in value)) throw new Error(`${path}.${key} is required`);
  }
}

function knownFields(
  value: JsonObject,
  fields: readonly string[],
  path: string
) {
  for (const key of Object.keys(value)) {
    if (!fields.includes(key))
      throw new Error(`${path} has unknown field ${key}`);
  }
}

function stringAt(value: unknown, path: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${path} must be a non-empty string`);
  }
  return value;
}

function integerAt(value: unknown, path: string, min: number, max: number) {
  if (
    !Number.isInteger(value) ||
    (value as number) < min ||
    (value as number) > max
  ) {
    throw new Error(`${path} must be an integer between ${min} and ${max}`);
  }
  return value as number;
}

function irisIntegerAt(value: unknown, path: string, min: number, max: number) {
  const normalized =
    typeof value === "string" && /^(0|[1-9][0-9]*)$/.test(value)
      ? Number(value)
      : value;
  return integerAt(normalized, path, min, max);
}

function decimalAt(value: unknown, path: string, positive = false): string {
  const text = stringAt(value, path);
  if (!/^(0|[1-9][0-9]*)$/.test(text) || (positive && text === "0")) {
    throw new Error(
      `${path} must be a ${positive ? "positive " : ""}decimal integer`
    );
  }
  const parsed = BigInt(text);
  if (parsed > 18_446_744_073_709_551_615n) {
    throw new Error(`${path} exceeds u64`);
  }
  return text;
}

export function hexAt(value: unknown, path: string, bytes: number): string {
  const text = stringAt(value, path);
  if (!new RegExp(`^0x[0-9a-fA-F]{${bytes * 2}}$`).test(text)) {
    throw new Error(`${path} must be 0x-prefixed ${bytes}-byte hex`);
  }
  return text;
}

function evmAddressAt(value: unknown, path: string): string {
  return hexAt(value, path, 20);
}

function hexBlobAt(value: unknown, path: string): string {
  const text = stringAt(value, path);
  if (!/^0x(?:[0-9a-fA-F]{2})+$/.test(text)) {
    throw new Error(`${path} must be 0x-prefixed even-length hex`);
  }
  return text;
}

function irisBytes32At(value: unknown, path: string): string {
  const text = stringAt(value, path);
  if (/^0x[0-9a-fA-F]{40}$/.test(text)) {
    return `0x${"0".repeat(24)}${text.slice(2)}`;
  }
  return hexAt(text, path, 32);
}

export function bytes32(value: string): Buffer {
  return Buffer.from(value.slice(2), "hex");
}

export async function retryPostStateRead<T>(
  read: (minContextSlot?: number) => Promise<T | null>,
  minContextSlot?: number,
  attempts = 10,
  delayMs = 500
): Promise<T> {
  let lastError: unknown;
  for (let attempt = 0; attempt < attempts; attempt++) {
    try {
      const value = await read(minContextSlot);
      if (value !== null) return value;
      lastError = undefined;
    } catch (error) {
      lastError = error;
    }
    if (attempt + 1 < attempts && delayMs > 0) {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, delayMs));
    }
  }
  if (lastError) throw lastError;
  throw new Error(`post-state account unavailable after ${attempts} attempts`);
}

export function parseCctpConfig(value: unknown): CctpConfig {
  const config = objectAt(value, "config");
  exactFields(
    config,
    ["rpcUrl", "payerKeypair", "source", "arbitrum", "destination"],
    "config"
  );
  const source = objectAt(config.source, "config.source");
  exactFields(
    source,
    [
      "ownerKeypair",
      "initiatorKeypair",
      "addressBookRootKeypair",
      "salt",
      "recipient",
      "targetToken",
      "targetChainId",
      "targetDecimals",
      "allowedSlippageBps",
      "inputAmount",
      "outputAmount",
      "protocolFeeAmount",
      "maxFee",
      "minFinalityThreshold",
    ],
    "config.source"
  );
  const arbitrum = objectAt(config.arbitrum, "config.arbitrum");
  exactFields(
    arbitrum,
    [
      "rpcUrl",
      "account",
      "canaryAddress",
      "messageTransmitter",
      "cctpAdapter",
      "cctpAdapterRuntimeHash",
    ],
    "config.arbitrum"
  );
  const destination = objectAt(config.destination, "config.destination");
  exactFields(
    destination,
    [
      "ownerKeypair",
      "salt",
      "sourceChainId",
      "remoteUsdc",
      "recipient",
      "executor",
      "amount",
    ],
    "config.destination"
  );
  const parsed: CctpConfig = {
    rpcUrl: stringAt(config.rpcUrl, "config.rpcUrl"),
    payerKeypair: stringAt(config.payerKeypair, "config.payerKeypair"),
    source: {
      ownerKeypair: stringAt(source.ownerKeypair, "config.source.ownerKeypair"),
      initiatorKeypair: stringAt(
        source.initiatorKeypair,
        "config.source.initiatorKeypair"
      ),
      addressBookRootKeypair: stringAt(
        source.addressBookRootKeypair,
        "config.source.addressBookRootKeypair"
      ),
      salt: hexAt(source.salt, "config.source.salt", 32),
      recipient: hexAt(source.recipient, "config.source.recipient", 32),
      targetToken: hexAt(source.targetToken, "config.source.targetToken", 32),
      targetChainId: integerAt(
        source.targetChainId,
        "config.source.targetChainId",
        1,
        Number.MAX_SAFE_INTEGER
      ),
      targetDecimals: integerAt(
        source.targetDecimals,
        "config.source.targetDecimals",
        1,
        255
      ),
      allowedSlippageBps: integerAt(
        source.allowedSlippageBps,
        "config.source.allowedSlippageBps",
        0,
        10_000
      ),
      inputAmount: decimalAt(
        source.inputAmount,
        "config.source.inputAmount",
        true
      ),
      outputAmount: decimalAt(
        source.outputAmount,
        "config.source.outputAmount",
        true
      ),
      protocolFeeAmount: decimalAt(
        source.protocolFeeAmount,
        "config.source.protocolFeeAmount"
      ),
      maxFee: decimalAt(source.maxFee, "config.source.maxFee"),
      minFinalityThreshold: integerAt(
        source.minFinalityThreshold,
        "config.source.minFinalityThreshold",
        1000,
        2000
      ),
    },
    arbitrum: {
      rpcUrl: stringAt(arbitrum.rpcUrl, "config.arbitrum.rpcUrl"),
      account: stringAt(arbitrum.account, "config.arbitrum.account"),
      canaryAddress: evmAddressAt(
        arbitrum.canaryAddress,
        "config.arbitrum.canaryAddress"
      ),
      messageTransmitter: evmAddressAt(
        arbitrum.messageTransmitter,
        "config.arbitrum.messageTransmitter"
      ),
      cctpAdapter: evmAddressAt(
        arbitrum.cctpAdapter,
        "config.arbitrum.cctpAdapter"
      ),
      cctpAdapterRuntimeHash: hexAt(
        arbitrum.cctpAdapterRuntimeHash,
        "config.arbitrum.cctpAdapterRuntimeHash",
        32
      ),
    },
    destination: {
      ownerKeypair: stringAt(
        destination.ownerKeypair,
        "config.destination.ownerKeypair"
      ),
      salt: hexAt(destination.salt, "config.destination.salt", 32),
      sourceChainId: integerAt(
        destination.sourceChainId,
        "config.destination.sourceChainId",
        1,
        Number.MAX_SAFE_INTEGER
      ),
      remoteUsdc: hexAt(
        destination.remoteUsdc,
        "config.destination.remoteUsdc",
        32
      ),
      recipient: stringAt(
        destination.recipient,
        "config.destination.recipient"
      ),
      executor: stringAt(destination.executor, "config.destination.executor"),
      amount: decimalAt(destination.amount, "config.destination.amount", true),
    },
  };
  if (![1000, 2000].includes(parsed.source.minFinalityThreshold)) {
    throw new Error("config.source.minFinalityThreshold must be 1000 or 2000");
  }
  if (parsed.source.maxFee !== "0") {
    throw new Error("config.source.maxFee must be 0 for Standard Transfer");
  }
  if (parsed.source.minFinalityThreshold !== 2000) {
    throw new Error(
      "config.source.minFinalityThreshold must be 2000 for Standard Transfer"
    );
  }
  if (parsed.source.targetChainId !== ARBITRUM_CHAIN_ID) {
    throw new Error("config.source.targetChainId must be 42161");
  }
  if (parsed.destination.sourceChainId !== ARBITRUM_CHAIN_ID) {
    throw new Error("config.destination.sourceChainId must be 42161");
  }
  if (parsed.source.targetDecimals !== 6) {
    throw new Error("config.source.targetDecimals must be 6");
  }
  if (!/^0x0{24}[0-9a-fA-F]{40}$/.test(parsed.source.recipient)) {
    throw new Error(
      "config.source.recipient must be a zero-left-padded EVM address"
    );
  }
  if (/^0x0{64}$/.test(parsed.source.recipient)) {
    throw new Error("config.source.recipient must not be the zero address");
  }
  if (parsed.source.targetToken.toLowerCase() !== ARBITRUM_USDC.toLowerCase()) {
    throw new Error(`config.source.targetToken must be ${ARBITRUM_USDC}`);
  }
  if (
    parsed.destination.remoteUsdc.toLowerCase() !== ARBITRUM_USDC.toLowerCase()
  ) {
    throw new Error(`config.destination.remoteUsdc must be ${ARBITRUM_USDC}`);
  }
  if (
    parsed.arbitrum.messageTransmitter.toLowerCase() !==
    EVM_MESSAGE_TRANSMITTER.toLowerCase()
  ) {
    throw new Error(
      `config.arbitrum.messageTransmitter must be ${EVM_MESSAGE_TRANSMITTER}`
    );
  }
  if (/^0x0{40}$/i.test(parsed.arbitrum.cctpAdapter)) {
    throw new Error("config.arbitrum.cctpAdapter must not be the zero address");
  }
  if (/^0x0{64}$/i.test(parsed.arbitrum.cctpAdapterRuntimeHash)) {
    throw new Error(
      "config.arbitrum.cctpAdapterRuntimeHash must not be the zero hash"
    );
  }
  for (const [path, amount] of [
    ["config.source.inputAmount", parsed.source.inputAmount],
    ["config.source.outputAmount", parsed.source.outputAmount],
    ["config.destination.amount", parsed.destination.amount],
  ] as const) {
    if (amount !== CCTP_CANARY_AMOUNT) {
      throw new Error(`${path} must be ${CCTP_CANARY_AMOUNT}`);
    }
  }
  if (parsed.source.protocolFeeAmount !== "0") {
    throw new Error("config.source.protocolFeeAmount must be 0");
  }
  if (parsed.source.allowedSlippageBps !== 0) {
    throw new Error("config.source.allowedSlippageBps must be 0");
  }
  const net =
    BigInt(parsed.source.inputAmount) - BigInt(parsed.source.protocolFeeAmount);
  if (net <= 0n)
    throw new Error(
      "config.source.protocolFeeAmount must be below inputAmount"
    );
  if (
    BigInt(parsed.source.maxFee) >= net ||
    BigInt(parsed.source.outputAmount) > net - BigInt(parsed.source.maxFee)
  ) {
    throw new Error(
      "config.source output must fit the net amount after maxFee"
    );
  }
  return parsed;
}

function optionalStage(
  value: unknown,
  path: string,
  fields: Readonly<Record<string, "string" | "hex" | "hex32">>
) {
  const stage = objectAt(value, path);
  knownFields(stage, Object.keys(fields), path);
  const parsed: Record<string, string> = {};
  for (const [name, kind] of Object.entries(fields)) {
    if (stage[name] === undefined) continue;
    parsed[name] =
      kind === "string"
        ? stringAt(stage[name], `${path}.${name}`)
        : kind === "hex32"
        ? hexAt(stage[name], `${path}.${name}`, 32)
        : hexBlobAt(stage[name], `${path}.${name}`);
  }
  return parsed;
}

export function parseCctpState(value: unknown): CctpState {
  const state = objectAt(value, "state");
  knownFields(
    state,
    ["version", "configPath", "source", "destination"],
    "state"
  );
  if (state.version !== 1) throw new Error("state.version must be 1");
  const result: Record<string, unknown> = { version: 1 };
  if (state.configPath !== undefined)
    result.configPath = stringAt(state.configPath, "state.configPath");
  if (state.source !== undefined) {
    result.source = optionalStage(state.source, "state.source", {
      setupSignature: "string",
      solanaSignature: "string",
      circleEventAccount: "string",
      message: "hex",
      attestation: "hex",
      nonce: "hex32",
      evmTransactionHash: "hex32",
    });
  }
  if (state.destination !== undefined) {
    result.destination = parseEvmDestinationState(state.destination);
  }
  return result as CctpState;
}

export function validateIrisResponse(
  value: unknown,
  expected: IrisExpectation
): Readonly<{ message: string; attestation: string; nonce: string }> {
  const response = objectAt(value, "Iris response");
  if (!Array.isArray(response.messages) || response.messages.length === 0) {
    throw new Error("Iris response.messages must contain a message");
  }
  const item = objectAt(response.messages[0], "Iris response.messages[0]");
  if (item.status !== "complete") {
    throw new Error(`Iris message status is ${String(item.status)}`);
  }
  const decoded = objectAt(
    item.decodedMessage,
    "Iris response.messages[0].decodedMessage"
  );
  const body = objectAt(
    decoded.decodedMessageBody,
    "Iris response.messages[0].decodedMessage.decodedMessageBody"
  );
  const sourceDomain = irisIntegerAt(
    decoded.sourceDomain,
    "Iris sourceDomain",
    0,
    4_294_967_295
  );
  const destinationDomain = irisIntegerAt(
    decoded.destinationDomain,
    "Iris destinationDomain",
    0,
    4_294_967_295
  );
  const mintRecipient = irisBytes32At(body.mintRecipient, "Iris mintRecipient");
  const amount = decimalAt(body.amount, "Iris amount", true);
  if (sourceDomain !== expected.sourceDomain) {
    throw new Error(
      `Iris sourceDomain ${sourceDomain} does not match expected ${expected.sourceDomain}`
    );
  }
  if (destinationDomain !== expected.destinationDomain) {
    throw new Error(
      `Iris destinationDomain ${destinationDomain} does not match expected ${expected.destinationDomain}`
    );
  }
  if (mintRecipient.toLowerCase() !== expected.mintRecipient.toLowerCase()) {
    throw new Error(
      `Iris mintRecipient ${mintRecipient} does not match expected ${expected.mintRecipient}`
    );
  }
  if (amount !== expected.amount) {
    throw new Error(
      `Iris amount ${amount} does not match expected ${expected.amount}`
    );
  }
  return {
    message: hexBlobAt(item.message, "Iris message"),
    attestation: hexBlobAt(item.attestation, "Iris attestation"),
    nonce: hexAt(decoded.nonce, "Iris nonce", 32),
  };
}

export function mergeIrisAttestation(
  stateValue: unknown,
  direction: "source" | "destination",
  response: unknown,
  expected: IrisExpectation
): CctpState {
  const state = parseCctpState(stateValue);
  const attestation = validateIrisResponse(response, expected);
  return direction === "source"
    ? {
        ...state,
        source: { ...state.source, ...attestation },
      }
    : {
        ...state,
        destination: { ...state.destination, ...attestation },
      };
}

function readCircleU64Slot(message: Buffer, offset: number) {
  if (!message.subarray(offset, offset + 24).equals(Buffer.alloc(24))) {
    throw new Error("persisted Circle amount slot exceeds u64");
  }
  return message.readBigUInt64BE(offset + 24);
}

export function validatePersistedCircleMessage(
  messageHex: string,
  expected: Readonly<{
    sourceDomain: number;
    destinationDomain: number;
    nonce: string;
    burnToken: string;
    mintRecipient: string;
    amount: bigint;
    maxFee: bigint;
    feeExecuted: bigint;
    minFinalityThreshold: number;
    finalityThresholdExecuted: number;
    messageRecipient?: string;
    destinationCaller?: string;
  }>
) {
  const message = Buffer.from(
    hexBlobAt(messageHex, "persisted Circle message").slice(2),
    "hex"
  );
  if (message.length < 376)
    throw new Error("persisted Circle message is truncated");
  const body = message.subarray(148);
  if (message.readUInt32BE(0) !== 1 || body.readUInt32BE(0) !== 1) {
    throw new Error("persisted Circle message version is unsupported");
  }
  if (message.readUInt32BE(4) !== expected.sourceDomain) {
    throw new Error(
      "persisted Circle source domain does not match current config"
    );
  }
  if (message.readUInt32BE(8) !== expected.destinationDomain) {
    throw new Error(
      "persisted Circle destination domain does not match current config"
    );
  }
  const matches = (actual: Uint8Array, value: string) =>
    Buffer.from(actual).equals(bytes32(value));
  if (!matches(message.subarray(12, 44), expected.nonce)) {
    throw new Error("persisted Circle nonce does not match state");
  }
  if (
    expected.messageRecipient &&
    !matches(message.subarray(76, 108), expected.messageRecipient)
  ) {
    throw new Error("persisted Circle message recipient does not match config");
  }
  if (
    expected.destinationCaller &&
    !matches(message.subarray(108, 140), expected.destinationCaller)
  ) {
    throw new Error(
      "persisted Circle destination caller does not match config"
    );
  }
  if (
    message.readUInt32BE(140) !== expected.minFinalityThreshold ||
    message.readUInt32BE(144) !== expected.finalityThresholdExecuted
  ) {
    throw new Error("persisted Circle finality does not match current config");
  }
  if (!matches(body.subarray(4, 36), expected.burnToken)) {
    throw new Error(
      "persisted Circle burn token does not match current config"
    );
  }
  if (!matches(body.subarray(36, 68), expected.mintRecipient)) {
    throw new Error(
      "persisted Circle mint recipient does not match current config"
    );
  }
  if (readCircleU64Slot(body, 68) !== expected.amount) {
    throw new Error("persisted Circle amount does not match current config");
  }
  if (readCircleU64Slot(body, 132) !== expected.maxFee) {
    throw new Error("persisted Circle max fee does not match current config");
  }
  if (readCircleU64Slot(body, 164) !== expected.feeExecuted) {
    throw new Error(
      "persisted Circle executed fee does not match current config"
    );
  }
}

export function toKitInstruction(
  instruction: web3.TransactionInstruction
): Instruction {
  return {
    programAddress: address(instruction.programId.toBase58()),
    accounts: instruction.keys.map((accountMeta) => ({
      address: address(accountMeta.pubkey.toBase58()),
      role: accountMeta.isSigner
        ? accountMeta.isWritable
          ? AccountRole.WRITABLE_SIGNER
          : AccountRole.READONLY_SIGNER
        : accountMeta.isWritable
        ? AccountRole.WRITABLE
        : AccountRole.READONLY,
    })),
    data: new Uint8Array(instruction.data),
  };
}

export async function compileNativeV1Transaction(
  instructions: readonly web3.TransactionInstruction[],
  signerBytes: readonly Uint8Array[],
  latestBlockhash: V1LatestBlockhash,
  config: Readonly<V1TransactionConfig> = NATIVE_V1_CONFIG
): Promise<
  Readonly<{
    transaction: Transaction;
    addressCount: number;
    size: number;
    config: Readonly<V1TransactionConfig>;
  }>
> {
  if (signerBytes.length === 0)
    throw new Error("at least one signer is required");
  if (config.computeUnitLimit === undefined) {
    throw new Error("native v1 computeUnitLimit must be explicit");
  }
  if (config.loadedAccountsDataSizeLimit === undefined) {
    throw new Error("native v1 loadedAccountsDataSizeLimit must be explicit");
  }
  const signers = await Promise.all(
    signerBytes.map((bytes) => createKeyPairSignerFromBytes(bytes))
  );
  const payer = signers[0];
  const kitInstructions = instructions.map(toKitInstruction);
  const inlineAddresses = new Set<string>([payer.address]);
  for (const instruction of kitInstructions) {
    inlineAddresses.add(instruction.programAddress);
    for (const accountMeta of instruction.accounts ?? []) {
      inlineAddresses.add(accountMeta.address);
    }
  }
  if (inlineAddresses.size > 64) {
    throw new Error(
      `native v1 transaction uses ${inlineAddresses.size} inline addresses; maximum is 64`
    );
  }
  const message = addSignersToTransactionMessage(
    signers,
    pipe(
      createTransactionMessage({ version: 1 }),
      (value) => setTransactionMessageFeePayerSigner(payer, value),
      (value) =>
        setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, value),
      (value) => appendTransactionMessageInstructions(kitInstructions, value),
      (value) => setTransactionMessageConfig(config, value)
    )
  );
  const compiledMessage = compileTransactionMessage(message);
  if (compiledMessage.staticAccounts.length !== inlineAddresses.size) {
    throw new Error("native v1 inline address assertion failed");
  }
  const transaction = await signTransactionMessageWithSigners(message);
  assertIsTransactionWithinSizeLimit(transaction);
  const size = getTransactionSize(transaction);
  if (size > 4096) {
    throw new Error(`native v1 transaction is ${size} bytes; maximum is 4096`);
  }
  return {
    transaction,
    addressCount: compiledMessage.staticAccounts.length,
    size,
    config,
  };
}

export async function selectDestinationSettlementMode(
  instructions: readonly web3.TransactionInstruction[],
  signerBytes: readonly Uint8Array[],
  latestBlockhash: V1LatestBlockhash
) {
  try {
    await compileNativeV1Transaction(
      instructions,
      signerBytes,
      latestBlockhash
    );
    return "combined" as const;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (
      /exceeds limit|size limit|inline addresses|maximum|too many account addresses/i.test(
        message
      )
    ) {
      return "fallback" as const;
    }
    throw error;
  }
}

export async function executeWithSendGate(
  send: boolean,
  operations: Readonly<{
    simulate: () => Promise<unknown>;
    send: () => Promise<string>;
    verify: (id: string) => Promise<unknown>;
  }>
): Promise<
  Readonly<{ mode: "simulation" }> | Readonly<{ mode: "sent"; id: string }>
> {
  await operations.simulate();
  if (!send) return { mode: "simulation" };
  const id = await operations.send();
  await operations.verify(id);
  return { mode: "sent", id };
}

export type CommandStage = Readonly<{
  simulate: () => Promise<unknown>;
  broadcast: () => Promise<string>;
  writeArtifact: (id: string) => Promise<unknown>;
  confirm: (id: string) => Promise<unknown>;
  verifyPostState: () => Promise<unknown>;
}>;

export async function executeCommandStage(send: boolean, stage: CommandStage) {
  await stage.simulate();
  if (!send) return { mode: "simulation" as const };
  const id = await stage.broadcast();
  await stage.writeArtifact(id);
  await stage.confirm(id);
  await stage.verifyPostState();
  return { mode: "sent" as const, id };
}

export async function executeDestinationSettlementWorkflow(
  options: Readonly<{
    send: boolean;
    combinedFits: boolean;
    alreadyReceived: boolean;
  }>,
  stages: Readonly<{
    combined: CommandStage;
    receive: CommandStage;
    prepareFinalize: () => Promise<CommandStage>;
    reportDeferredFinalize: () => void;
  }>
) {
  if (options.combinedFits || options.alreadyReceived) {
    return executeCommandStage(options.send, stages.combined);
  }
  if (!options.send) {
    await stages.receive.simulate();
    stages.reportDeferredFinalize();
    return { mode: "simulation" as const, finalizeDeferred: true as const };
  }
  const receive = await executeCommandStage(true, stages.receive);
  const finalizeStage = await stages.prepareFinalize();
  const finalize = await executeCommandStage(true, finalizeStage);
  if (receive.mode !== "sent" || finalize.mode !== "sent") {
    throw new Error("destination fallback send did not return signatures");
  }
  return {
    mode: "sent" as const,
    receiveId: receive.id,
    finalizeId: finalize.id,
  };
}

export async function verifyEvmBoundary(
  expected: Readonly<{ expectedChainId: bigint; expectedSigner?: string }>,
  boundary: Readonly<{
    readChainId: () => Promise<bigint>;
    resolveSigner?: () => Promise<string>;
  }>
) {
  const chainId = await boundary.readChainId();
  if (chainId !== expected.expectedChainId) {
    throw new Error(
      `EVM RPC chain id ${chainId} does not match expected ${expected.expectedChainId}`
    );
  }
  if (!expected.expectedSigner) return { chainId };
  if (!boundary.resolveSigner)
    throw new Error("cast signer resolver is required");
  const signer = await boundary.resolveSigner();
  if (signer.toLowerCase() !== expected.expectedSigner.toLowerCase()) {
    throw new Error(
      `cast account resolves to ${signer}, expected ${expected.expectedSigner}`
    );
  }
  return { chainId, signer: expected.expectedSigner };
}

export type AttemptStatus =
  | "none"
  | "pending"
  | "failed"
  | "expired"
  | "confirmed";

export function planDestinationResumption(
  state: Readonly<{
    usedNonce: boolean;
    receiptFinalized: boolean;
    receiveAttempt: AttemptStatus;
    finalizeAttempt: AttemptStatus;
  }>
) {
  if (state.receiptFinalized) return "complete" as const;
  if (state.finalizeAttempt === "pending") {
    throw new Error("destination finalization attempt is still pending");
  }
  if (state.finalizeAttempt === "confirmed") {
    throw new Error(
      "confirmed finalization attempt has no fill receipt post-state"
    );
  }
  if (state.usedNonce) return "finalize-only" as const;
  if (state.receiveAttempt === "pending") {
    throw new Error("destination receive attempt is still pending");
  }
  if (state.receiveAttempt === "confirmed") {
    throw new Error("confirmed receive attempt has no used nonce post-state");
  }
  return "receive-and-finalize" as const;
}
