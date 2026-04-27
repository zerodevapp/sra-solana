use anchor_lang::prelude::*;

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
    /// Adapter program that IX 2 will CPI into. Must be a member of
    /// `sra_config.allowed_adapters` at the time of this emission.
    pub adapter_program: Pubkey,
    /// Pre-increment nonce captured before IX 1 bumps `mint_nonce`.
    pub nonce: u64,
    /// Tokens pulled from the owner ATA into the SRA vault and forwarded
    /// to adapter custody in the same transaction.
    pub input_amount: u64,
    /// Minimum output amount the destination must honor; enforced here via
    /// the slippage check and re-honored by the target-specific adapter flow.
    pub output_amount: u64,
    /// Immutable target chain id from SraConfig, copied into the event so
    /// relayers do not need an extra account read to route the deposit.
    pub target_chain_id: u64,
    /// Opaque 32-byte target handler identifier. EVM handlers are left-padded
    /// addresses; non-EVM targets may use all 32 bytes.
    pub target_handler: [u8; 32],
    /// Opaque 32-byte target adapter identifier. EVM adapters are left-padded
    /// addresses; non-EVM targets may use all 32 bytes.
    pub target_adapter: [u8; 32],
    /// Destination account / recipient identifier.
    pub recipient: [u8; 32],
    /// App identifier copied from SraConfig.
    pub app_id: [u8; 32],
    /// Per-deposit target token identifier. Stored as raw bytes32 so Solana
    /// pubkeys and other non-EVM ids are not truncated.
    pub target_token: [u8; 32],
    /// Action indices selected for the destination execution.
    pub action_idxs: Vec<u64>,
}
