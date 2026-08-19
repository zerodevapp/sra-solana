use anchor_lang::prelude::*;

#[error_code]
pub enum AcrossAdapterError {
    #[msg("Failed to serialize deposit-now seed data")]
    DepositNowSeedSerializationFailed,

    #[msg("Mint nonce does not contain a prepared deposit")]
    PreparedNonceUnavailable,

    #[msg("Delegate account does not match the derived Across PDA")]
    InvalidDelegateAccount,

    #[msg("No tokens in adapter custody to recover")]
    NothingToRecover,

    #[msg("Failed to serialize relay data for the relay hash")]
    RelayHashSerializationFailed,

    #[msg("Across fill status account has an invalid layout")]
    InvalidFillStatus,

    #[msg("Across fill is not in the completed state")]
    FillNotCompleted,

    #[msg("Relay data does not re-derive the provided relay hash")]
    RelayHashMismatch,

    #[msg("Fill recipient does not match the route")]
    RecipientMismatch,

    #[msg("Fill output token does not match the route mint")]
    OutputTokenMismatch,

    #[msg("Fill origin chain does not match the route source chain")]
    OriginChainMismatch,

    #[msg("Across fill status account has not been closed")]
    FillStatusNotClosed,

    #[msg("Across fill deadline has not passed")]
    FillDeadlineNotPassed,

    #[msg("Across proof does not authenticate the executed output amount")]
    ExecutedAmountUnavailable,
}
