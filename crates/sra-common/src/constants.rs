use anchor_lang::prelude::Pubkey;
use anchor_lang::pubkey;

/// Max adapter program IDs per SraConfig or SolanaSraRoute.
/// Solana-specific: bounds Vec<Pubkey> size for rent calculation.
/// EVM has no equivalent limit (uses linked list).
pub const MAX_ADAPTERS: usize = 8;

/// Max executor pubkeys per route (bounds account size); distinct from `MAX_ADAPTERS`.
pub const MAX_EXECUTORS: usize = 8;

/// Max actions in a destination route's committed `action_config`.
/// Solana-specific: bounds the route account size and the per-fill action list
/// re-supplied as instruction data. EVM has no equivalent limit.
pub const MAX_ACTION_IDXS: usize = 4;

/// Max account slots in a `VerifiedCpi` action's committed account template.
/// Bounds the per-action template (and thus the route's committed action config)
/// for both `InitSpace` and DoS, and gives the executor a fixed ceiling when it
/// resolves the template against the supplied accounts. Sized to cover the
/// heaviest planned inner CPI — its named accounts plus relayer-supplied routing
/// slots; confirm against the real inner-CPI account count before committing a
/// new dynamic-CPI template.
pub const MAX_TEMPLATE_SLOTS: usize = 32;

/// Basis points denominator (100% = 10,000 bps).
/// Matches EVM `BPS_DENOMINATOR` in Constants.sol.
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Max fee in basis points (10%). Bounds the default fee, the per-deposit
/// override, and per-mint tier brackets. Matches the EVM `MAX_FEE_BPS` bound
/// enforced in BaseHandler.sol `setProtocolFee` / `setAssetFeeConfigs`.
pub const MAX_FEE_BPS: u16 = 1_000;

/// Sentinel for "no per-deposit fee override".
pub const NO_FEE_BPS_OVERRIDE: u16 = u16::MAX;

/// Max slippage in basis points (100%).
/// Matches the EVM `slippageBps <= BPS_DENOMINATOR` bound enforced in
/// SRAHandler.sol `_onInstall` / `setSlippagePolicy`.
pub const MAX_SLIPPAGE_BPS: u16 = BPS_DENOMINATOR;

/// Protocol-internal Solana identity: the self-route guard in
/// `SourceHandlerData::validate` and the signed-quote domain separator. The
/// value is Across's registered Solana chain id — the Across adapter's relay
/// hash depends on it, so it must equal the SVM Spoke Pool's `state.chain_id`
/// (pinned by the destination Surfpool E2E). CCTP corridors are keyed by
/// [`cctp_domain_for_chain`] instead; both adapters are in service.
///
/// Network-selected: mainnet by default, devnet under `--features devnet`.
#[cfg(not(feature = "devnet"))]
pub const SOLANA_CHAIN_ID: u64 = 34_268_394_551_451;
// TODO(devnet): VERIFY against the devnet SVM Spoke Pool's on-chain `state.chain_id`
// (and Across's published devnet chain ID) before relying on the destination fill
// path — the relay hash is derived from this value. Sourced from docs.across.to's
// Solana Devnet row; unverified on-chain.
#[cfg(feature = "devnet")]
pub const SOLANA_CHAIN_ID: u64 = 133_268_194_659_241;

/// USDC mint on Solana mainnet. Not compiled into program logic (client/test use
/// only) — the mint is always supplied as a runtime account, so devnet uses a
/// devnet mint without a rebuild.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

/// Across SVM SpokePool program ID. Enforced on-chain via `#[account(address = ...)]`
/// and folded into the deposit-delegate PDA seed, so it is baked into the binary.
/// Network-selected: mainnet by default, devnet under `--features devnet`.
#[cfg(not(feature = "devnet"))]
pub const ACROSS_SPOKE_POOL_ID: Pubkey = pubkey!("DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru");
#[cfg(feature = "devnet")]
pub const ACROSS_SPOKE_POOL_ID: Pubkey = pubkey!("JAZWcGrpSWNPTBj8QtJ9UyQqhJCDhG9GJkDeMf5NQBiq");

/// Min Across `fill_deadline_offset` (seconds): too short forces instant expiry
/// and strands the refund in `adapter_custody`.
pub const MIN_FILL_DEADLINE_OFFSET: u32 = 300;

/// Circle CCTP v2 program IDs — identical on devnet and mainnet, so not
/// network-selected. Enforced on-chain via `#[account(address = ...)]` in the
/// CCTP adapter.
pub const CCTP_TOKEN_MESSENGER_MINTER_V2_ID: Pubkey =
    pubkey!("CCTPV2vPZJS2u2BBsUoscuikbYjnpFmbFsvVuJdgUMQe");
pub const CCTP_MESSAGE_TRANSMITTER_V2_ID: Pubkey =
    pubkey!("CCTPV2Sm4AdWt5296sk4P66VBZ7bEhcARwFaaS9YPbeC");

/// Solana's own CCTP domain — never a destination in the chain ↔ domain table.
pub const SOLANA_CCTP_DOMAIN: u32 = 5;

/// CCTP v2 `min_finality_threshold` bounds: 1000 = fast transfer (soft
/// finality, fee ≥ 1bp), 2000 = standard transfer (hard finality).
pub const FINALITY_THRESHOLD_FAST: u32 = 1000;
pub const FINALITY_THRESHOLD_FINALIZED: u32 = 2000;

/// EVM chain id ↔ CCTP domain rows for the active network. Mainnet rows mirror
/// sra-stx `CCTPAdapter.getDomainForChain` exactly, including its deliberate
/// BNB (56) omission — CCTP v2 on BNB is USYC-only and SRA is USDC-only.
/// One table backs both lookup directions, so the inverse cannot drift.
#[cfg(not(feature = "devnet"))]
const CCTP_CHAIN_DOMAINS: &[(u64, u32)] = &[
    (1, 0),       // Ethereum
    (43_114, 1),  // Avalanche
    (10, 2),      // OP Mainnet
    (42_161, 3),  // Arbitrum One
    (8_453, 6),   // Base
    (137, 7),     // Polygon PoS
    (130, 10),    // Unichain
    (59_144, 11), // Linea
    (146, 13),    // Sonic
    (480, 14),    // World Chain
    (999, 19),    // HyperEVM
    (57_073, 21), // Ink
];
#[cfg(feature = "devnet")]
const CCTP_CHAIN_DOMAINS: &[(u64, u32)] = &[
    (11_155_111, 0), // Ethereum Sepolia
    (43_113, 1),     // Avalanche Fuji
    (11_155_420, 2), // OP Sepolia
    (421_614, 3),    // Arbitrum Sepolia
    (84_532, 6),     // Base Sepolia
    (80_002, 7),     // Polygon Amoy
];

/// CCTP destination domain for an EVM chain id; `None` = unsupported corridor.
pub fn cctp_domain_for_chain(chain_id: u64) -> Option<u32> {
    CCTP_CHAIN_DOMAINS
        .iter()
        .find_map(|&(chain, domain)| (chain == chain_id).then_some(domain))
}

/// Exact inverse of [`cctp_domain_for_chain`] (same table, keyed by domain).
pub fn chain_id_for_cctp_domain(domain: u32) -> Option<u64> {
    CCTP_CHAIN_DOMAINS
        .iter()
        .find_map(|&(chain, d)| (d == domain).then_some(chain))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the network-selected constants to the active build feature. Fails if
    /// the `devnet` feature-forward is broken (wrong network's constant compiled in)
    /// — run under both `cargo test` and `cargo test --features devnet`.
    #[test]
    fn network_constants_match_active_feature() {
        #[cfg(feature = "devnet")]
        {
            assert_eq!(
                ACROSS_SPOKE_POOL_ID,
                pubkey!("JAZWcGrpSWNPTBj8QtJ9UyQqhJCDhG9GJkDeMf5NQBiq")
            );
            assert_eq!(SOLANA_CHAIN_ID, 133_268_194_659_241);
            assert_eq!(cctp_domain_for_chain(421_614), Some(3)); // Arb Sepolia
            assert_eq!(cctp_domain_for_chain(42_161), None); // mainnet row absent
        }
        #[cfg(not(feature = "devnet"))]
        {
            assert_eq!(
                ACROSS_SPOKE_POOL_ID,
                pubkey!("DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru")
            );
            assert_eq!(SOLANA_CHAIN_ID, 34_268_394_551_451);
            assert_eq!(cctp_domain_for_chain(42_161), Some(3)); // Arbitrum One
            assert_eq!(cctp_domain_for_chain(421_614), None); // devnet row absent
        }
    }

    #[test]
    fn cctp_domain_table_round_trips() {
        for &(chain, domain) in CCTP_CHAIN_DOMAINS {
            assert_eq!(cctp_domain_for_chain(chain), Some(domain));
            assert_eq!(chain_id_for_cctp_domain(domain), Some(chain));
        }
    }

    /// Find-first lookups are only an exact inverse if both columns are unique.
    #[test]
    fn cctp_domain_table_columns_are_unique() {
        for (i, &(chain, domain)) in CCTP_CHAIN_DOMAINS.iter().enumerate() {
            for &(other_chain, other_domain) in &CCTP_CHAIN_DOMAINS[i + 1..] {
                assert_ne!(chain, other_chain);
                assert_ne!(domain, other_domain);
            }
        }
    }

    #[test]
    fn cctp_domain_rejects_unmapped_chains() {
        // BNB is the deliberate omission (CCTP v2 there is USYC-only).
        assert_eq!(cctp_domain_for_chain(56), None);
        assert_eq!(cctp_domain_for_chain(0), None);
        assert_eq!(cctp_domain_for_chain(SOLANA_CHAIN_ID), None);
    }

    #[test]
    fn solana_domain_never_maps_to_an_evm_chain() {
        assert_eq!(chain_id_for_cctp_domain(SOLANA_CCTP_DOMAIN), None);
    }
}
