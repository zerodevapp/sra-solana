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
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub target_decimals: u8,
}

/// Emitted by `validate_and_prepare` once the owner's tokens are in adapter
/// custody. Off-chain relayers combine these raw routing fields with the
/// immutable SraConfig to build whatever target-specific bridge message IX 2
/// requires, whether the destination uses EVM addresses or full 32-byte IDs.
#[event]
pub struct DepositPrepared {
    /// SraConfig PDA the deposit was prepared for.
    pub sra_config: Pubkey,
    /// Source mint that was locked.
    pub source_mint: Pubkey,
    /// Adapter program that IX 2 will CPI into. Must be a member of the
    /// immutable `handler_data.allowed_adapters` committed by the SRA.
    pub adapter_program: Pubkey,
    /// Pre-increment nonce captured before IX 1 bumps `mint_nonce`.
    pub nonce: u64,
    /// Tokens pulled from the owner ATA into the SRA vault and forwarded
    /// to adapter custody in the same transaction.
    pub input_amount: u64,
    /// Minimum output amount the destination must honor; enforced here via
    /// the slippage check and re-honored by the target-specific adapter flow.
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
