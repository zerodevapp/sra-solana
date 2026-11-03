import {
  decimalAt,
  exactFields,
  hexAt,
  hexBlobAt,
  knownFields,
  nonzeroHexAt,
  objectAt,
  stringAt,
} from "./cctp-mainnet-e2e-json";

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

export type CctpSourceState = Readonly<{
  setupSignature?: string;
  solanaSignature?: string;
  circleEventAccount?: string;
  message?: string;
  attestation?: string;
  nonce?: string;
  evmTransactionHash?: string;
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

export type CctpState = Readonly<{
  version: 1;
  configPath?: string;
  source?: CctpSourceState;
  destination?: EvmDestinationState;
}>;

type Mutable<T> = { -readonly [Field in keyof T]: T[Field] };

function attemptAt(value: unknown, path: string): EvmAttempt {
  const attempt = objectAt(value, path);
  exactFields(attempt, ["hash", "senderNonce", "submittedBlock"], path);
  return {
    hash: nonzeroHexAt(attempt.hash, `${path}.hash`, 32),
    senderNonce: decimalAt(attempt.senderNonce, `${path}.senderNonce`),
    submittedBlock: decimalAt(attempt.submittedBlock, `${path}.submittedBlock`),
  };
}

function attemptHistoryAt(value: unknown, path: string): readonly EvmAttempt[] {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  return value.map((attempt, index) => attemptAt(attempt, `${path}[${index}]`));
}

function setupAttemptsAt(value: unknown, path: string): EvmSetupAttempts {
  const setup = objectAt(value, path);
  knownFields(setup, ["deployment", "initiator", "funding"], path);
  const parsed: Mutable<EvmSetupAttempts> = {};
  if (setup.deployment !== undefined) {
    parsed.deployment = attemptHistoryAt(
      setup.deployment,
      `${path}.deployment`
    );
  }
  if (setup.initiator !== undefined) {
    parsed.initiator = attemptHistoryAt(setup.initiator, `${path}.initiator`);
  }
  if (setup.funding !== undefined) {
    parsed.funding = attemptHistoryAt(setup.funding, `${path}.funding`);
  }
  return parsed;
}

function stringHistoryAt(value: unknown, path: string): readonly string[] {
  if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
  return value.map((entry, index) => stringAt(entry, `${path}[${index}]`));
}

function parseSourceState(value: unknown, path: string): CctpSourceState {
  const source = objectAt(value, path);
  knownFields(
    source,
    [
      "setupSignature",
      "solanaSignature",
      "circleEventAccount",
      "message",
      "attestation",
      "nonce",
      "evmTransactionHash",
    ],
    path
  );
  const parsed: Mutable<CctpSourceState> = {};
  if (source.setupSignature !== undefined) {
    parsed.setupSignature = stringAt(
      source.setupSignature,
      `${path}.setupSignature`
    );
  }
  if (source.solanaSignature !== undefined) {
    parsed.solanaSignature = stringAt(
      source.solanaSignature,
      `${path}.solanaSignature`
    );
  }
  if (source.circleEventAccount !== undefined) {
    parsed.circleEventAccount = stringAt(
      source.circleEventAccount,
      `${path}.circleEventAccount`
    );
  }
  if (source.message !== undefined) {
    parsed.message = hexBlobAt(source.message, `${path}.message`);
  }
  if (source.attestation !== undefined) {
    parsed.attestation = hexBlobAt(source.attestation, `${path}.attestation`);
  }
  if (source.nonce !== undefined) {
    parsed.nonce = hexAt(source.nonce, `${path}.nonce`, 32);
  }
  if (source.evmTransactionHash !== undefined) {
    parsed.evmTransactionHash = hexAt(
      source.evmTransactionHash,
      `${path}.evmTransactionHash`,
      32
    );
  }
  return parsed;
}

function parseDestinationState(
  value: unknown,
  path: string
): EvmDestinationState {
  const destination = objectAt(value, path);
  knownFields(
    destination,
    [
      "setupSignature",
      "evmBurnTransactionHash",
      "arbitrumSraAddress",
      "arbitrumSetupAttempts",
      "arbitrumDepositAttempt",
      "arbitrumDepositAttemptHistory",
      "arbitrumBaselineDepositNonce",
      "arbitrumConfirmedDepositNonce",
      "arbitrumBurnMessage",
      "message",
      "attestation",
      "nonce",
      "solanaReceiveSignature",
      "solanaReceiveLastValidBlockHeight",
      "solanaFinalizeSignature",
      "solanaFinalizeLastValidBlockHeight",
      "solanaActionsSignature",
      "solanaRecoverSignature",
      "arbitrumRecoveryAttemptHistory",
      "solanaRecoverySignatureHistory",
    ],
    path
  );
  const parsed: Mutable<EvmDestinationState> = {};
  if (destination.setupSignature !== undefined) {
    parsed.setupSignature = stringAt(
      destination.setupSignature,
      `${path}.setupSignature`
    );
  }
  if (destination.evmBurnTransactionHash !== undefined) {
    parsed.evmBurnTransactionHash = hexAt(
      destination.evmBurnTransactionHash,
      `${path}.evmBurnTransactionHash`,
      32
    );
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
  if (destination.arbitrumDepositAttempt !== undefined) {
    parsed.arbitrumDepositAttempt = attemptAt(
      destination.arbitrumDepositAttempt,
      `${path}.arbitrumDepositAttempt`
    );
  }
  if (destination.arbitrumDepositAttemptHistory !== undefined) {
    parsed.arbitrumDepositAttemptHistory = attemptHistoryAt(
      destination.arbitrumDepositAttemptHistory,
      `${path}.arbitrumDepositAttemptHistory`
    );
  }
  if (destination.arbitrumBaselineDepositNonce !== undefined) {
    parsed.arbitrumBaselineDepositNonce = decimalAt(
      destination.arbitrumBaselineDepositNonce,
      `${path}.arbitrumBaselineDepositNonce`
    );
  }
  if (destination.arbitrumConfirmedDepositNonce !== undefined) {
    parsed.arbitrumConfirmedDepositNonce = decimalAt(
      destination.arbitrumConfirmedDepositNonce,
      `${path}.arbitrumConfirmedDepositNonce`
    );
  }
  if (destination.arbitrumBurnMessage !== undefined) {
    parsed.arbitrumBurnMessage = hexBlobAt(
      destination.arbitrumBurnMessage,
      `${path}.arbitrumBurnMessage`
    );
  }
  if (destination.message !== undefined) {
    parsed.message = hexBlobAt(destination.message, `${path}.message`);
  }
  if (destination.attestation !== undefined) {
    parsed.attestation = hexBlobAt(
      destination.attestation,
      `${path}.attestation`
    );
  }
  if (destination.nonce !== undefined) {
    parsed.nonce = hexAt(destination.nonce, `${path}.nonce`, 32);
  }
  if (destination.solanaReceiveSignature !== undefined) {
    parsed.solanaReceiveSignature = stringAt(
      destination.solanaReceiveSignature,
      `${path}.solanaReceiveSignature`
    );
  }
  if (destination.solanaReceiveLastValidBlockHeight !== undefined) {
    parsed.solanaReceiveLastValidBlockHeight = decimalAt(
      destination.solanaReceiveLastValidBlockHeight,
      `${path}.solanaReceiveLastValidBlockHeight`
    );
  }
  if (destination.solanaFinalizeSignature !== undefined) {
    parsed.solanaFinalizeSignature = stringAt(
      destination.solanaFinalizeSignature,
      `${path}.solanaFinalizeSignature`
    );
  }
  if (destination.solanaFinalizeLastValidBlockHeight !== undefined) {
    parsed.solanaFinalizeLastValidBlockHeight = decimalAt(
      destination.solanaFinalizeLastValidBlockHeight,
      `${path}.solanaFinalizeLastValidBlockHeight`
    );
  }
  if (destination.solanaActionsSignature !== undefined) {
    parsed.solanaActionsSignature = stringAt(
      destination.solanaActionsSignature,
      `${path}.solanaActionsSignature`
    );
  }
  if (destination.solanaRecoverSignature !== undefined) {
    parsed.solanaRecoverSignature = stringAt(
      destination.solanaRecoverSignature,
      `${path}.solanaRecoverSignature`
    );
  }
  if (destination.arbitrumRecoveryAttemptHistory !== undefined) {
    parsed.arbitrumRecoveryAttemptHistory = attemptHistoryAt(
      destination.arbitrumRecoveryAttemptHistory,
      `${path}.arbitrumRecoveryAttemptHistory`
    );
  }
  if (destination.solanaRecoverySignatureHistory !== undefined) {
    parsed.solanaRecoverySignatureHistory = stringHistoryAt(
      destination.solanaRecoverySignatureHistory,
      `${path}.solanaRecoverySignatureHistory`
    );
  }
  if (
    (parsed.evmBurnTransactionHash !== undefined ||
      parsed.arbitrumDepositAttempt !== undefined) &&
    (parsed.evmBurnTransactionHash === undefined ||
      parsed.arbitrumDepositAttempt === undefined ||
      parsed.evmBurnTransactionHash.toLowerCase() !==
        parsed.arbitrumDepositAttempt.hash.toLowerCase())
  ) {
    throw new Error(
      `${path}.evmBurnTransactionHash must equal ${path}.arbitrumDepositAttempt.hash`
    );
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
  const parsed: Mutable<CctpState> = { version: 1 };
  if (state.configPath !== undefined) {
    parsed.configPath = stringAt(state.configPath, "state.configPath");
  }
  if (state.source !== undefined) {
    parsed.source = parseSourceState(state.source, "state.source");
  }
  if (state.destination !== undefined) {
    parsed.destination = parseDestinationState(
      state.destination,
      "state.destination"
    );
  }
  return parsed;
}
