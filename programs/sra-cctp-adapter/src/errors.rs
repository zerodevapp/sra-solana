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

    // Destination-leg variants are appended (never reordered) so deployed
    // error codes stay stable.
    #[msg("CCTP message bytes do not parse as a supported burn message")]
    MalformedMessage,

    #[msg("Attestation does not verify against the enabled attesters")]
    InvalidAttestation,
}
