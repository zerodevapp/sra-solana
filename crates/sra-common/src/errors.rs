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

    // -- General --
    #[msg("Slippage bps exceeds maximum")]
    InvalidSlippage,

    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,

    // -- Source: paired instruction attestation --
    #[msg("Expected neighboring paired instruction is missing")]
    MissingPairedInstruction,

    #[msg("Paired instruction is malformed or has the wrong type")]
    InvalidPairedInstruction,

    #[msg("Paired instruction does not match the expected deposit intent")]
    PairedInstructionMismatch,

    #[msg("Source handler data does not match immutable SRA identity")]
    InvalidHandlerData,

    #[msg("Across fill deadline offset below minimum")]
    FillDeadlineTooShort,

    #[msg("Across exclusivity period exceeds fill deadline offset")]
    ExclusivityExceedsDeadline,

    #[msg("Deposit routing does not match the committed SRA route")]
    RouteMismatch,

    #[msg("Pending admin must be non-default")]
    InvalidPendingAdmin,

    // -- Source: protocol fee collection --
    #[msg("Attested protocol fee does not match the resolved fee")]
    FeeMismatch,

    #[msg("Fee recipient token account required when the fee is positive")]
    MissingFeeRecipientAccount,

    #[msg("Input amount must be greater than zero")]
    ZeroInputAmount,
    // -- Source: install/route field validation --
    #[msg("Routing field is zero or routes to the local Solana chain")]
    InvalidRouteField,

    #[msg("output_amount exceeds the u128 maximum (upper 16 bytes must be zero)")]
    OutputAmountOverflow,

    // -- Destination: route creation --
    #[msg("Supplied route_config_hash does not match the recomputed route config")]
    InvalidRouteConfig,

    #[msg("Committed action bps (SplTransfer amounts + VerifiedCpi inputs) must sum to exactly 100% of the vault")]
    InvalidActionBpsSum,

    #[msg("Action config field is invalid (zero key or template missing a required fixed slot)")]
    InvalidActionConfig,

    #[msg("VerifiedCpi account template exceeds the maximum slot count")]
    TooManyTemplateSlots,

    #[msg("Adapter already present in the allowlist")]
    DuplicateAdapter,

    // -- Destination: fill finalization --
    #[msg("Route is paused")]
    RoutePaused,

    // -- Destination: fund recovery --
    #[msg("No funds available to recover")]
    NothingToRecover,

    // -- Destination: action execution --
    #[msg("Action's supplied accounts do not match its required accounts")]
    ActionAccountMismatch,

    #[msg("Action variant is not executable by this program")]
    UnsupportedAction,

    // -- Destination: verified-CPI engine --
    #[msg("Verified-CPI program is not the pinned program or is not a deployed executable")]
    ProgramNotPinned,

    #[msg("Verified-CPI output destination is not the route owner's output token account")]
    DestinationNotPinned,

    #[msg("Verified-CPI input_mint must equal the route's expected_mint")]
    InvalidInputMint,

    #[msg("Verified-CPI output gained less than the minimum acceptable amount")]
    MinOutNotMet,

    #[msg("Verified-CPI scratch account did not fully sweep back to the vault")]
    ScratchNotEmpty,

    #[msg("Signed price quote has expired")]
    PriceQuoteExpired,

    #[msg("No matching authorized signed price quote was found in the transaction")]
    InvalidPriceSignature,

    #[msg("Output mint is permissioned (permanent-delegate / pausable / default-frozen) and the action did not opt in")]
    PermissionedOutputNotAllowed,

    // -- Destination: executor allowlist --
    #[msg("Caller is not in the route's allowed_executors list")]
    ExecutorNotAllowed,

    #[msg("Allowed executors list exceeds maximum")]
    TooManyExecutors,

    #[msg("Executor pubkey must be non-zero")]
    InvalidExecutor,

    #[msg("Executor already present in the allowlist")]
    DuplicateExecutor,

    // -- Source: sponsored-deposit admin gate --
    #[msg("Sponsored deposits are not enabled by the fee admin")]
    SponsoredNotAllowed,

    // -- Destination: verified-CPI (appended to keep existing error codes stable) --
    #[msg("Verified-CPI callee did not consume the full staged input")]
    InputNotConsumed,

    #[msg("Initiator pubkey must be non-zero")]
    InvalidInitiator,

    #[msg("SRA is inactive")]
    SraInactive,

    #[msg("Caller is not the configured initiator")]
    InitiatorNotAllowed,

    #[msg("SRA vault balance is below the requested input amount")]
    InsufficientSourceBalance,

    #[msg("Verified-CPI callee changed scratch token-account control")]
    ScratchControlChanged,
}
