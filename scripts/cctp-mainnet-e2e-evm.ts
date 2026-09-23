type JsonObject = Record<string, unknown>;

export type EvmAttempt = Readonly<{
  hash: string;
  senderNonce: string;
  submittedBlock: string;
}>;

export type EvmSetupAttempts = Readonly<{
  deployment?: readonly EvmAttempt[];
  initiator?: readonly EvmAttempt[];
  funding?: readonly EvmAttempt[];
}>;

export type EvmDestinationState = Readonly<{
  setupSignature?: string;
  evmBurnTransactionHash?: string;
  arbitrumSraAddress?: string;
  arbitrumSetupAttempts?: EvmSetupAttempts;
  arbitrumDepositAttempt?: EvmAttempt;
  arbitrumDepositAttemptHistory?: readonly EvmAttempt[];
  arbitrumBaselineDepositNonce?: string;
  arbitrumConfirmedDepositNonce?: string;
  arbitrumBurnMessage?: string;
  message?: string;
  attestation?: string;
  nonce?: string;
  solanaReceiveSignature?: string;
  solanaReceiveLastValidBlockHeight?: string;
  solanaFinalizeSignature?: string;
  solanaFinalizeLastValidBlockHeight?: string;
  solanaActionsSignature?: string;
  solanaRecoverSignature?: string;
  arbitrumRecoveryAttemptHistory?: readonly EvmAttempt[];
  solanaRecoverySignatureHistory?: readonly string[];
}>;

function objectAt(value: unknown, path: string): JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  return value as JsonObject;
}

function knownFields(
  value: JsonObject,
  fields: readonly string[],
  path: string
) {
  for (const field of Object.keys(value)) {
    if (!fields.includes(field)) {
      throw new Error(`${path} has unknown field ${field}`);
    }
  }
}

function stringAt(value: unknown, path: string) {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${path} must be a non-empty string`);
  }
  return value;
}

function hexAt(value: unknown, path: string, bytes: number) {
  const text = stringAt(value, path);
  if (!new RegExp(`^0x[0-9a-fA-F]{${bytes * 2}}$`).test(text)) {
    throw new Error(`${path} must be 0x-prefixed ${bytes}-byte hex`);
  }
  return text;
}

function nonzeroHexAt(value: unknown, path: string, bytes: number) {
  const text = hexAt(value, path, bytes);
  if (text.toLowerCase() === `0x${"00".repeat(bytes)}`) {
    throw new Error(`${path} must not be zero`);
  }
  return text;
}

function hexBlobAt(value: unknown, path: string) {
  const text = stringAt(value, path);
  if (!/^0x(?:[0-9a-fA-F]{2})+$/.test(text)) {
    throw new Error(`${path} must be 0x-prefixed even-length hex`);
  }
  return text;
}

function decimalAt(value: unknown, path: string) {
  const text = stringAt(value, path);
  if (!/^(0|[1-9][0-9]*)$/.test(text)) {
    throw new Error(`${path} must be a decimal integer`);
  }
  return text;
}

function attemptAt(value: unknown, path: string): EvmAttempt {
  const attempt = objectAt(value, path);
  knownFields(attempt, ["hash", "senderNonce", "submittedBlock"], path);
  for (const field of ["hash", "senderNonce", "submittedBlock"] as const) {
    if (!(field in attempt)) throw new Error(`${path}.${field} is required`);
  }
  return {
    hash: nonzeroHexAt(attempt.hash, `${path}.hash`, 32),
    senderNonce: decimalAt(attempt.senderNonce, `${path}.senderNonce`),
    submittedBlock: decimalAt(attempt.submittedBlock, `${path}.submittedBlock`),
  };
}

function attemptHistoryAt(value: unknown, path: string) {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  return value.map((attempt, index) => attemptAt(attempt, `${path}[${index}]`));
}

function setupAttemptsAt(value: unknown, path: string): EvmSetupAttempts {
  const setup = objectAt(value, path);
  knownFields(setup, ["deployment", "initiator", "funding"], path);
  const parsed: {
    deployment?: readonly EvmAttempt[];
    initiator?: readonly EvmAttempt[];
    funding?: readonly EvmAttempt[];
  } = {};
  for (const stage of ["deployment", "initiator", "funding"] as const) {
    if (setup[stage] !== undefined) {
      parsed[stage] = attemptHistoryAt(setup[stage], `${path}.${stage}`);
    }
  }
  return parsed;
}

function stringHistoryAt(value: unknown, path: string) {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  return value.map((entry, index) => stringAt(entry, `${path}[${index}]`));
}

export function parseEvmDestinationState(
  value: unknown,
  path = "state.destination"
): EvmDestinationState {
  const destination = objectAt(value, path);
  const scalarFields = {
    setupSignature: "string",
    evmBurnTransactionHash: "hex32",
    arbitrumBaselineDepositNonce: "decimal",
    arbitrumConfirmedDepositNonce: "decimal",
    arbitrumBurnMessage: "hex",
    message: "hex",
    attestation: "hex",
    nonce: "hex32",
    solanaReceiveSignature: "string",
    solanaReceiveLastValidBlockHeight: "decimal",
    solanaFinalizeSignature: "string",
    solanaFinalizeLastValidBlockHeight: "decimal",
    solanaActionsSignature: "string",
    solanaRecoverSignature: "string",
  } as const;
  knownFields(
    destination,
    [
      ...Object.keys(scalarFields),
      "arbitrumSraAddress",
      "arbitrumSetupAttempts",
      "arbitrumDepositAttempt",
      "arbitrumDepositAttemptHistory",
      "arbitrumRecoveryAttemptHistory",
      "solanaRecoverySignatureHistory",
    ],
    path
  );
  const parsed: Record<string, unknown> = {};
  for (const [field, kind] of Object.entries(scalarFields)) {
    if (destination[field] === undefined) continue;
    parsed[field] =
      kind === "string"
        ? stringAt(destination[field], `${path}.${field}`)
        : kind === "hex32"
        ? hexAt(destination[field], `${path}.${field}`, 32)
        : kind === "hex"
        ? hexBlobAt(destination[field], `${path}.${field}`)
        : decimalAt(destination[field], `${path}.${field}`);
  }
  if (destination.arbitrumSraAddress !== undefined) {
    parsed.arbitrumSraAddress = nonzeroHexAt(
      destination.arbitrumSraAddress,
      `${path}.arbitrumSraAddress`,
      20
    );
  }
  if (destination.arbitrumSetupAttempts !== undefined) {
    parsed.arbitrumSetupAttempts = setupAttemptsAt(
      destination.arbitrumSetupAttempts,
      `${path}.arbitrumSetupAttempts`
    );
  }
  for (const field of [
    "arbitrumDepositAttemptHistory",
    "arbitrumRecoveryAttemptHistory",
  ] as const) {
    if (destination[field] !== undefined) {
      parsed[field] = attemptHistoryAt(destination[field], `${path}.${field}`);
    }
  }
  if (destination.arbitrumDepositAttempt !== undefined) {
    parsed.arbitrumDepositAttempt = attemptAt(
      destination.arbitrumDepositAttempt,
      `${path}.arbitrumDepositAttempt`
    );
  }
  if (destination.solanaRecoverySignatureHistory !== undefined) {
    parsed.solanaRecoverySignatureHistory = stringHistoryAt(
      destination.solanaRecoverySignatureHistory,
      `${path}.solanaRecoverySignatureHistory`
    );
  }
  const burnHash = parsed.evmBurnTransactionHash;
  const depositAttempt = parsed.arbitrumDepositAttempt as
    | EvmAttempt
    | undefined;
  if (
    (burnHash !== undefined || depositAttempt !== undefined) &&
    (typeof burnHash !== "string" ||
      depositAttempt === undefined ||
      burnHash.toLowerCase() !== depositAttempt.hash.toLowerCase())
  ) {
    throw new Error(
      `${path}.evmBurnTransactionHash must equal ${path}.arbitrumDepositAttempt.hash`
    );
  }
  return parsed;
}

export const ARBITRUM_SRA_DEPLOYMENT = {
  factory: "0x3E2746a51F205357F3915a7d4de38eEf0637b950",
  handler: "0x09bE9c9dd3219e81cA2F41AEF85481f86595f592",
  implementation: "0xb04b9916bd4bc76d87c2a1f295fb00f4db7b8abc",
  addressBook: "0x4B2Bf84F9109ab502Af5bA28B0bE945890acE154",
  resolver: "0xF8226B1F0832f48bF0b23c9ce72cBEDd94d4CD5a",
} as const;

export const ARBITRUM_CCTP_DEPLOYMENT = {
  chainId: 42161n,
  usdc: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
  tokenMessenger: "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d",
  messageTransmitter: "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64",
  owner: "0x9775137314fE595c943712B0b336327dfa80aE8A",
  localDomain: 3n,
  solanaDomain: 5n,
} as const;

export type ArbitrumCctpAdapterObservation = Readonly<{
  chainId: bigint;
  address: string;
  code: string;
  runtimeHash: string;
  handler: string;
  tokenMessenger: string;
  messageTransmitter: string;
  usdc: string;
  owner: string;
  localDomain: bigint;
  solanaDomain: bigint;
}>;

export type ArbitrumAdapterConfig = Readonly<{
  cctpAdapter: string;
  cctpAdapterRuntimeHash: string;
}>;

function sameHex(left: string, right: string) {
  return left.toLowerCase() === right.toLowerCase();
}

export function validateArbitrumCctpAdapter(
  config: ArbitrumAdapterConfig,
  observation: ArbitrumCctpAdapterObservation
) {
  if (observation.chainId !== ARBITRUM_CCTP_DEPLOYMENT.chainId) {
    throw new Error("wrong Arbitrum chain id");
  }
  if (!sameHex(observation.address, config.cctpAdapter)) {
    throw new Error("CCTP adapter address does not match config");
  }
  if (!/^0x(?:[0-9a-fA-F]{2})+$/.test(observation.code)) {
    throw new Error("CCTP adapter has no bytecode");
  }
  if (!sameHex(observation.runtimeHash, config.cctpAdapterRuntimeHash)) {
    throw new Error("CCTP adapter runtime hash does not match config");
  }
  if (!sameHex(observation.handler, ARBITRUM_SRA_DEPLOYMENT.handler)) {
    throw new Error("CCTP adapter handler does not match the pinned handler");
  }
  if (
    !sameHex(
      observation.tokenMessenger,
      ARBITRUM_CCTP_DEPLOYMENT.tokenMessenger
    )
  ) {
    throw new Error("CCTP adapter TokenMessenger does not match Circle");
  }
  if (
    !sameHex(
      observation.messageTransmitter,
      ARBITRUM_CCTP_DEPLOYMENT.messageTransmitter
    )
  ) {
    throw new Error("CCTP adapter MessageTransmitter does not match Circle");
  }
  if (!sameHex(observation.usdc, ARBITRUM_CCTP_DEPLOYMENT.usdc)) {
    throw new Error("CCTP adapter USDC does not match Arbitrum USDC");
  }
  if (!sameHex(observation.owner, ARBITRUM_CCTP_DEPLOYMENT.owner)) {
    throw new Error("CCTP adapter owner does not match the deployment owner");
  }
  if (observation.localDomain !== ARBITRUM_CCTP_DEPLOYMENT.localDomain) {
    throw new Error("CCTP adapter local domain is not Arbitrum");
  }
  if (observation.solanaDomain !== ARBITRUM_CCTP_DEPLOYMENT.solanaDomain) {
    throw new Error("CCTP adapter Solana domain is not 5");
  }
}
