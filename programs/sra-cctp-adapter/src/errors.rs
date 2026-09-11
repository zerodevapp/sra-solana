use anchor_lang::prelude::*;

#[error_code]
pub enum CctpAdapterError {
    #[msg("SRA nonce does not contain a triggered deposit")]
    TriggeredNonceUnavailable,

    #[msg("No tokens in adapter custody to recover")]
    NothingToRecover,

    #[msg("Target chain has no CCTP domain mapping")]
    UnsupportedChain,

    #[msg("CCTP finality threshold must be 1000 or 2000")]
    InvalidFinalityThreshold,

    #[msg("Committed output exceeds the net input after the CCTP max fee")]
    QuoteExceedsNetAfterFee,

    // Destination-leg variants are appended (never reordered) so deployed
    // error codes stay stable.
    #[msg("CCTP message bytes do not parse as a supported burn message")]
    MalformedMessage,

    #[msg("Attestation does not verify against the enabled attesters")]
    InvalidAttestation,

    #[msg("MessageTransmitter state account is malformed or inconsistent")]
    InvalidTransmitterState,

    #[msg("Used-nonce receipt does not prove an executed mint")]
    NonceNotUsed,

    #[msg("Message nonce does not match the supplied nonce")]
    NonceMismatch,

    #[msg("Message recipient is not the CCTP TokenMessengerMinter program")]
    InvalidRecipientProgram,

    #[msg("Message destination domain is not this chain's CCTP domain")]
    DestinationDomainMismatch,

    #[msg("Mint recipient is not the route vault for the expected mint")]
    MintRecipientMismatch,

    #[msg("Message source domain does not map to the route's source chain")]
    SourceDomainMismatch,

    #[msg("Executed fee exceeds max fee, or max fee is not below the amount")]
    FeeExceedsMaxFee,

    #[msg("CCTP input mint must be canonical six-decimal Solana USDC")]
    InvalidInputMint,
}
