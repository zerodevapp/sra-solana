use anchor_lang::prelude::*;

#[error_code]
pub enum CctpAdapterError {
    #[msg("Mint nonce does not contain a prepared deposit")]
    PreparedNonceUnavailable,

    #[msg("No tokens in adapter custody to recover")]
    NothingToRecover,

    #[msg("Target chain has no CCTP domain mapping")]
    UnsupportedChain,

    #[msg("CCTP finality threshold must be within 1..=2000")]
    InvalidFinalityThreshold,

    #[msg("Committed output exceeds the net input after the CCTP max fee")]
    QuoteExceedsNetAfterFee,
}
