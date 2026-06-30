use anchor_lang::prelude::Pubkey;
use anchor_lang::pubkey;

/// Max adapter program IDs per SraConfig or SolanaSraRoute.
/// Solana-specific: bounds Vec<Pubkey> size for rent calculation.
/// EVM has no equivalent limit (uses linked list).
pub const MAX_ADAPTERS: usize = 8;

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

/// Solana chain ID as registered in the Across protocol.
pub const SOLANA_CHAIN_ID: u64 = 1_151_111_081_099_592;

/// USDC mint on Solana mainnet.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

/// Across SVM SpokePool program ID on mainnet.
pub const ACROSS_SPOKE_POOL_ID: Pubkey = pubkey!("DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru");

/// Min Across `fill_deadline_offset` (seconds): too short forces instant expiry
/// and strands the refund in `adapter_custody`.
pub const MIN_FILL_DEADLINE_OFFSET: u32 = 300;
