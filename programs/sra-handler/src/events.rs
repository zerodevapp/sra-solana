use anchor_lang::prelude::*;

#[event]
pub struct SraInitialized {
    pub sra_config: Pubkey,
    pub owner: Pubkey,
    pub final_handler: Pubkey,
    pub handler_data_hash: [u8; 32],
    pub salt: [u8; 32],
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
    pub app_id: [u8; 32],
    pub default_fee_bps: u16,
    pub allow_override: bool,
}

#[event]
pub struct ProtocolFeeRemoved {
    pub app_id: [u8; 32],
}

#[event]
pub struct AssetFeeTierUpdated {
    pub app_id: [u8; 32],
    pub mint: Pubkey,
    pub threshold: u64,
    pub below_bps: u16,
    pub above_or_equal_bps: u16,
}

#[event]
pub struct AssetFeeTierRemoved {
    pub app_id: [u8; 32],
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

/// Emitted by `validate_and_prepare` after funds reach adapter custody. Carries
/// the routing inputs needed to build the target-specific bridge message.
#[event]
pub struct DepositPrepared {
    /// SraConfig PDA the deposit was prepared for.
    pub sra_config: Pubkey,
    /// Source mint that was locked.
    pub source_mint: Pubkey,
    /// Allowlisted adapter program for the prepared deposit.
    pub adapter_program: Pubkey,
    /// Nonce captured before the prepare handler increments it.
    pub nonce: u64,
    /// Gross tokens pulled from the owner ATA into the SRA vault. The amount
    /// forwarded to adapter custody and bridged in the same transaction is
    /// `input_amount - protocol_fee_amount`.
    pub input_amount: u64,
    /// Protocol fee collected on the source (v15). Zero when fees are
    /// disabled globally, the app has no fee config, or the deposit is
    /// sponsored. The EVM `ProtocolFeeApplied` event collapses into this.
    pub protocol_fee_amount: u64,
    /// Minimum output amount the destination must honor; enforced here via
    /// the slippage check (floored from the GROSS input, TOB-SRA-5 parity)
    /// and re-honored by the target-specific adapter flow.
    pub output_amount: u64,
    /// Runtime target chain id from `PrepareParams`.
    pub target_chain_id: u64,
    /// Opaque 32-byte target handler identifier. EVM handlers are left-padded
    /// addresses; non-EVM targets may use all 32 bytes.
    pub target_handler: [u8; 32],
    /// Opaque 32-byte target adapter identifier. EVM adapters are left-padded
    /// addresses; non-EVM targets may use all 32 bytes.
    pub target_adapter: [u8; 32],
    /// Destination account / recipient identifier.
    pub recipient: [u8; 32],
    /// Runtime app identifier from `PrepareParams`.
    pub app_id: [u8; 32],
    /// Per-deposit target token identifier. Stored as raw bytes32 so Solana
    /// pubkeys and other non-EVM ids are not truncated.
    pub target_token: [u8; 32],
    /// Action indices selected for the destination execution.
    pub action_idxs: Vec<u64>,
}
