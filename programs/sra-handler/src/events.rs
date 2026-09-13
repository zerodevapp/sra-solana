use anchor_lang::prelude::*;

#[event]
pub struct SraInitialized {
    pub sra_config: Pubkey,
    pub owner: Pubkey,
    pub final_handler: Pubkey,
    pub handler_data_hash: [u8; 32],
    pub salt: [u8; 32],
    pub initiator: Pubkey,
    pub active: bool,
    pub deposit_nonce: u64,
    pub initial_source_mint: Pubkey,
    pub initial_source_vault: Pubkey,
}

#[event]
pub struct SraManaged {
    pub sra_config: Pubkey,
    pub owner: Pubkey,
    pub initiator: Pubkey,
    pub active: bool,
    pub deposit_nonce: u64,
}

#[event]
pub struct SourceVaultRecovered {
    pub sra_config: Pubkey,
    pub owner: Pubkey,
    pub source_mint: Pubkey,
    pub amount: u64,
    pub deposit_nonce: u64,
}

#[event]
pub struct AddressBookRootInitialized {
    pub address_book_root: Pubkey,
    pub owner: Pubkey,
}

#[event]
pub struct AddressBookEntrySet {
    pub address_book_root: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub target_decimals: u8,
}

#[event]
pub struct FeeConfigInitialized {
    pub fee_config: Pubkey,
    pub admin: Pubkey,
    pub fee_recipient: Pubkey,
}

#[event]
pub struct FeeRecipientUpdated {
    pub old_fee_recipient: Pubkey,
    pub new_fee_recipient: Pubkey,
}

#[event]
pub struct ProtocolFeeUpdated {
    pub default_fee_bps: u16,
    pub allow_override: bool,
    pub allow_sponsored: bool,
}

#[event]
pub struct ProtocolFeeRemoved {}

#[event]
pub struct AssetFeeTierUpdated {
    pub mint: Pubkey,
    pub threshold: u64,
    pub below_bps: u16,
    pub above_or_equal_bps: u16,
}

#[event]
pub struct AssetFeeTierRemoved {
    pub mint: Pubkey,
}

#[event]
pub struct PendingAdminSet {
    pub admin: Pubkey,
    pub pending_admin: Pubkey,
}

#[event]
pub struct AdminTransferred {
    pub old_admin: Pubkey,
    pub new_admin: Pubkey,
}

/// Emitted by `create_route`. Carries the full route identity (post-write) for indexers.
#[event]
pub struct RouteCreated {
    pub route: Pubkey,
    pub owner: Pubkey,
    pub source_chain_id: u64,
    pub expected_mint: Pubkey,
    pub salt: [u8; 32],
    pub allowed_adapters: Vec<Pubkey>,
    pub allowed_executors: Vec<Pubkey>,
    pub paused: bool,
}

/// Emitted by `manage_route` with the post-write allowlists and paused flag.
#[event]
pub struct RouteManaged {
    pub route: Pubkey,
    pub owner: Pubkey,
    pub allowed_adapters: Vec<Pubkey>,
    pub allowed_executors: Vec<Pubkey>,
    pub paused: bool,
}

/// Emitted by `finalize_fill` once the verified amount is staged in the
/// execution vault and the replay-marker receipt is written. Carries the
/// post-write fill lifecycle state for indexers. There are no fees on the
/// destination (collected on the source), so none appear here.
#[event]
pub struct FillFinalized {
    pub route: Pubkey,
    pub fill_id: [u8; 32],
    /// Amount moved route_vault -> execution_vault (the full verified amount).
    pub amount: u64,
    /// Source-committed minimum the fill had to satisfy.
    pub expected_amount: u64,
    pub execution_status: sra_common::types::ExecutionStatus,
    pub finalized_at: i64,
}

/// Emitted by `recover_funds` when the execution vault is drained to the owner.
#[event]
pub struct FundsRecovered {
    pub route: Pubkey,
    pub owner: Pubkey,
    pub fill_id: [u8; 32],
    /// Amount drained to the owner.
    pub amount: u64,
}

/// Emitted by `execute_actions` when every committed action applied and the
/// receipt advanced to `ActionsSucceeded` (terminal).
#[event]
pub struct ActionsExecuted {
    pub route: Pubkey,
    pub fill_id: [u8; 32],
}

/// Emitted by `execute_actions` / `retry_actions` when an action could not be
/// applied; the receipt is `ActionsFailed` and funds stay staged for retry or
/// recovery. No transfers from this run persist.
#[event]
pub struct ActionsFailed {
    pub route: Pubkey,
    pub fill_id: [u8; 32],
}

/// Emitted by `retry_actions` on a successful re-run (receipt -> `ActionsSucceeded`).
/// Distinct from `ActionsExecuted` so indexers see the success came via a retry.
#[event]
pub struct ActionsRetried {
    pub route: Pubkey,
    pub fill_id: [u8; 32],
}

#[event]
pub struct DepositTriggered {
    pub sra_config: Pubkey,
    pub initiator: Pubkey,
    pub source_mint: Pubkey,
    pub adapter_program: Pubkey,
    pub nonce: u64,
    pub input_amount: u64,
    pub protocol_fee_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub recipient: [u8; 32],
    pub target_token: [u8; 32],
}
