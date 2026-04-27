use anchor_lang::prelude::*;

#[error_code]
pub enum AcrossAdapterError {
    #[msg("Failed to serialize deposit-now seed data")]
    DepositNowSeedSerializationFailed,
}
