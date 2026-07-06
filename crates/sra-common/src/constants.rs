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

/// Max fee in basis points (10%). Applies to both protocol and app fees.
/// Matches EVM `require(_feeBps <= 1000)` in BaseHandler.sol:267,275.
pub const MAX_FEE_BPS: u16 = 1_000;

/// Sentinel for "no per-deposit fee override".
pub const NO_FEE_BPS_OVERRIDE: u16 = u16::MAX;

/// Max slippage in basis points (100%).
/// Matches EVM `require(slippageBps <= BPS_DENOMINATOR)` in SRAHandler.sol:78.
pub const MAX_SLIPPAGE_BPS: u16 = BPS_DENOMINATOR;

/// Solana chain ID as registered in the Across protocol — MUST equal the
/// deployed SVM Spoke Pool's `state.chain_id` (it is bound into every relay
/// hash) and Across's published `CHAIN_IDs.SOLANA`. Pinned by the destination
/// Surfpool E2E, where the real program rejects fills whose relay hash was
/// derived from any other value.
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
        }
        #[cfg(not(feature = "devnet"))]
        {
            assert_eq!(
                ACROSS_SPOKE_POOL_ID,
                pubkey!("DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru")
            );
            assert_eq!(SOLANA_CHAIN_ID, 34_268_394_551_451);
        }
    }
}
