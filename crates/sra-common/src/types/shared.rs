use anchor_lang::prelude::*;

/// Global fee state: admin + the single fee recipient.
#[account]
#[derive(InitSpace)]
pub struct FeeConfig {
    pub admin: Pubkey,
    /// Pubkey::default() = none (2-step admin transfer).
    pub pending_admin: Pubkey,
    /// Pubkey::default() = fees disabled globally.
    pub fee_recipient: Pubkey,
    pub bump: u8,
}

/// Single global protocol fee (EVM `protocolFeeConfig`). Deposits are the fee
/// payer, never a payee — all fees go to `FeeConfig.fee_recipient`.
/// PDA seeds: ["protocol_fee"]
#[account]
#[derive(InitSpace)]
pub struct ProtocolFeeConfig {
    /// Fallback when no asset tier matches; <= MAX_FEE_BPS.
    pub default_fee_bps: u16,
    /// Honor a per-deposit bps override (EVM `allowOverride`).
    pub allow_override: bool,
    /// Honor a per-deposit `is_sponsor` fee skip (EVM `allowSponsored`).
    pub allow_sponsored: bool,
    pub bump: u8,
}

/// Per-mint volume tier (EVM `assetFeeConfigs[asset]`).
/// PDA seeds: ["asset_fee_tier", mint]
#[account]
#[derive(InitSpace)]
pub struct AssetFeeTier {
    pub mint: Pubkey,
    /// Amount boundary in the mint's native units.
    pub threshold: u64,
    /// Fee bps when amount < threshold; <= MAX_FEE_BPS.
    pub below_bps: u16,
    /// Fee bps when amount >= threshold; <= MAX_FEE_BPS.
    pub above_or_equal_bps: u16,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_config_init_space() {
        // 32 admin + 32 pending_admin + 32 fee_recipient + 1 bump = 97
        assert_eq!(FeeConfig::INIT_SPACE, 97);
    }

    #[test]
    fn protocol_fee_config_init_space() {
        // 2 default_fee_bps + 1 allow_override + 1 allow_sponsored + 1 bump = 5
        assert_eq!(ProtocolFeeConfig::INIT_SPACE, 5);
    }

    #[test]
    fn asset_fee_tier_init_space() {
        // 32 mint + 8 threshold + 2 below_bps + 2 above_or_equal_bps + 1 bump = 45
        assert_eq!(AssetFeeTier::INIT_SPACE, 45);
    }
}
