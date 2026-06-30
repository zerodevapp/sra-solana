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

    // -- Source: paired instruction attestation (appended to keep prior codes stable) --
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

    // Reserved — no longer raised (any app_id is registrable, EVM parity);
    // kept so later codes stay stable.
    #[msg("app_id must be non-zero")]
    InvalidAppId,

    #[msg("Pending admin must be non-default")]
    InvalidPendingAdmin,

    // -- Source: protocol fee collection (appended to keep prior codes stable) --
    #[msg("Attested protocol fee does not match the resolved fee")]
    FeeMismatch,

    #[msg("Fee recipient token account required when the fee is positive")]
    MissingFeeRecipientAccount,

    #[msg("Input amount must be greater than zero")]
    ZeroInputAmount,
    // -- Source: install/route field validation (appended to keep prior codes stable) --
    #[msg("Routing field is zero or routes to the local Solana chain")]
    InvalidRouteField,

    #[msg("output_amount exceeds the u128 maximum (upper 16 bytes must be zero)")]
    OutputAmountOverflow,

    // -- Destination: route creation (appended to keep prior codes stable) --
    #[msg("Supplied route_config_hash does not match the recomputed route config")]
    InvalidRouteConfig,

    #[msg("SplTransfer amount_bps sum exceeds 100% of the vault")]
    ActionBpsSumExceeded,

    #[msg("Action config field is invalid (zero key, bps over 100%, or template missing a required fixed slot)")]
    InvalidActionConfig,

    #[msg("VerifiedCpi account template exceeds the maximum slot count")]
    TooManyTemplateSlots,

    #[msg("Adapter already present in the allowlist")]
    DuplicateAdapter,

    // -- Destination: fill finalization (appended to keep prior codes stable) --
    #[msg("Route is paused")]
    RoutePaused,

    // -- Destination: fund recovery (appended to keep prior codes stable) --
    #[msg("No funds available to recover")]
    NothingToRecover,

    // -- Destination: action execution (appended to keep prior codes stable) --
    #[msg("Supplied action config does not match the route's committed action config hash")]
    ActionConfigHashMismatch,

    #[msg("Action's supplied accounts do not match its required accounts")]
    ActionAccountMismatch,

    #[msg("Action variant is not executable by this program")]
    UnsupportedAction,

    // -- Destination: verified-CPI engine (appended to keep prior codes stable) --
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

    // -- Destination: executor allowlist (appended to keep prior codes stable) --
    #[msg("Caller is not in the route's allowed_executors list")]
    ExecutorNotAllowed,

    #[msg("Allowed executors list exceeds maximum")]
    TooManyExecutors,

    #[msg("Executor pubkey must be non-zero")]
    InvalidExecutor,

    #[msg("Executor already present in the allowlist")]
    DuplicateExecutor,
}
