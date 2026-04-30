use anchor_lang::prelude::*;

#[error_code]
pub enum AcrossAdapterError {
    #[msg("Failed to serialize deposit-now seed data")]
    DepositNowSeedSerializationFailed,

    #[msg("Mint nonce does not contain a prepared deposit")]
    PreparedNonceUnavailable,

    #[msg("Delegate account does not match the derived Across PDA")]
    InvalidDelegateAccount,
}
