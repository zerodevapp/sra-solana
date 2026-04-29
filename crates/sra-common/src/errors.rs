use anchor_lang::prelude::*;

#[error_code]
pub enum SraError {
    // -- Source: adapter / route validation --
    #[msg("Adapter program not in allowed_adapters list")]
    AdapterNotAllowed,

    #[msg("Adapter must be a deployed program account")]
    InvalidAdapterProgram,

    #[msg("Output amount below minimum after slippage")]
    SlippageExceeded,

    #[msg("Address book entry is inactive or does not match")]
    InvalidAddressBook,

    #[msg("target_token must be non-zero")]
    InvalidTokenInfo,

    // -- Source: message / config --
    #[msg("action_idxs exceeds maximum allowed")]
    TooManyActionIdxs,

    #[msg("Allowed adapters list exceeds maximum")]
    TooManyAdapters,

    // -- Destination: fill validation --
    #[msg("VerifiedFill does not match expected route")]
    InvalidFill,

    #[msg("Fill amount below expected amount")]
    FillAmountTooLow,

    #[msg("Fill already finalized (replay detected)")]
    ReplayDetected,

    #[msg("Vault balance insufficient for fill amount")]
    InsufficientVaultBalance,

    #[msg("VerifiedFill mint does not match route expected_mint")]
    MintMismatch,

    #[msg("VerifiedFill source_chain_id does not match route")]
    ChainMismatch,

    // -- Destination: execution status --
    #[msg("Invalid execution status for this operation")]
    InvalidExecutionStatus,

    // -- Fees --
    #[msg("Fee exceeds maximum allowed (10%)")]
    FeeTooHigh,

    // -- Source: paired instruction attestation --
    #[msg("Expected neighboring paired instruction is missing")]
    MissingPairedInstruction,

    #[msg("Paired instruction is malformed or has the wrong type")]
    InvalidPairedInstruction,

    #[msg("Paired instruction does not match the expected deposit intent")]
    PairedInstructionMismatch,

    // -- Adapter: fill verification --
    #[msg("Across fill status is not Filled")]
    FillNotCompleted,

    #[msg("Relay hash does not match derived value")]
    RelayHashMismatch,

    // -- General --
    #[msg("Slippage bps exceeds maximum")]
    InvalidSlippage,

    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,

    #[msg("bytes32 value cannot be narrowed to a 20-byte EVM address")]
    InvalidEvmAddress,
}
