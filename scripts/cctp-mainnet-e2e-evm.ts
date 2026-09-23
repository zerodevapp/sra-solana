import { keccak_256 } from "@noble/hashes/sha3";
import { hexBlobAt } from "./cctp-mainnet-e2e-json";

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

function runtimeHash(code: string) {
  const bytes = Buffer.from(code.slice(2), "hex");
  return `0x${Buffer.from(keccak_256(bytes)).toString("hex")}`;
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
  const code = hexBlobAt(observation.code, "CCTP adapter bytecode");
  if (!sameHex(runtimeHash(code), config.cctpAdapterRuntimeHash)) {
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
