use anchor_lang::prelude::*;

/// Global fee config — used by destination finalize_fill.
/// PDA seeds: ["fee_config"]
#[account]
#[derive(InitSpace)]
pub struct FeeConfig {
    pub admin: Pubkey,
    pub protocol_fee_bps: u16,
    pub protocol_fee_recipient: Pubkey,
    pub bump: u8,
}

/// Per-app fee. PDA seeds: ["app_fee", app_id]
/// Constraint: protocol_fee_bps + app_fee_bps <= MAX_FEE_BPS (1000 = 10%)
#[account]
#[derive(InitSpace)]
pub struct AppFeeEntry {
    pub app_id: [u8; 32],
    pub app_fee_bps: u16,
    pub app_fee_recipient: Pubkey,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_config_init_space() {
        // 32 admin + 2 protocol_fee_bps + 32 protocol_fee_recipient + 1 bump = 67
        assert_eq!(FeeConfig::INIT_SPACE, 67);
    }

    #[test]
    fn app_fee_entry_init_space() {
        // 32 app_id + 2 app_fee_bps + 32 app_fee_recipient + 1 bump = 67
        assert_eq!(AppFeeEntry::INIT_SPACE, 67);
    }
}
