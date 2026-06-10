use anchor_lang::prelude::Pubkey;
use anchor_lang::pubkey;

/// Max adapter program IDs per SraConfig or SolanaSraRoute.
/// Solana-specific: bounds Vec<Pubkey> size for rent calculation.
/// EVM has no equivalent limit (uses linked list).
pub const MAX_ADAPTERS: usize = 8;

/// Max action indices per source message or destination route.
/// Solana-specific: bounds ABI-encoded message size to fit 1,232-byte tx limit.
/// EVM has no equivalent limit.
pub const MAX_ACTION_IDXS: usize = 4;

/// Basis points denominator (100% = 10,000 bps).
/// Matches EVM `BPS_DENOMINATOR` in Constants.sol.
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Max fee in basis points (10%). Applies to both protocol and app fees.
/// Matches EVM `require(_feeBps <= 1000)` in BaseHandler.sol:267,275.
pub const MAX_FEE_BPS: u16 = 1_000;

/// Max slippage in basis points (100%).
/// Matches EVM `require(slippageBps <= BPS_DENOMINATOR)` in SRAHandler.sol:78.
pub const MAX_SLIPPAGE_BPS: u16 = BPS_DENOMINATOR;

/// Solana chain ID as registered in the Across protocol.
pub const SOLANA_CHAIN_ID: u64 = 1_151_111_081_099_592;

/// USDC mint on Solana mainnet.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

/// Across SVM SpokePool program ID on mainnet.
pub const ACROSS_SPOKE_POOL_ID: Pubkey = pubkey!("DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru");

/// M-05: Minimum Across `fill_deadline_offset` (seconds). A shorter offset is a
/// forced-expiry red flag — a tiny deadline makes the deposit expire instantly,
/// triggering an Across refund to `adapter_custody` where funds get stranded.
pub const MIN_FILL_DEADLINE_OFFSET: u32 = 300;

/// M-05: Maximum Across `fill_deadline_offset` (seconds, 6h). Generous upper
/// bound; anything larger is rejected as unreasonable.
pub const MAX_FILL_DEADLINE_OFFSET: u32 = 21_600;
