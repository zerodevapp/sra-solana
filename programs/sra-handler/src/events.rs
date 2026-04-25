use anchor_lang::prelude::*;

/// Emitted by `validate_and_prepare` once the owner's tokens are in adapter
/// custody and the canonical ABI message has been built. Off-chain relayers
/// consume this event to submit the bridge deposit on the adapter side in
/// the paired IX 2 — the bytes must be byte-identical to what IX 2 re-derives.
#[event]
pub struct DepositPrepared {
    /// SraConfig PDA the deposit was prepared for.
    pub sra_config: Pubkey,
    /// Source mint that was locked.
    pub source_mint: Pubkey,
    /// Adapter program that IX 2 will CPI into. Must be a member of
    /// `sra_config.allowed_adapters` at the time of this emission.
    pub adapter_program: Pubkey,
    /// Pre-increment nonce baked into the canonical message. IX 2 re-derives
    /// the same message using `mint_nonce.nonce - 1` (because IX 1 bumped it).
    pub nonce: u64,
    /// Tokens pulled from the owner ATA into the SRA vault and forwarded
    /// to adapter custody in the same transaction.
    pub input_amount: u64,
    /// Minimum output amount the destination must honor; enforced here via
    /// the slippage check and re-honored on the EVM side.
    pub output_amount: u64,
    /// Canonical `abi.encode(adapter, handler, sourceChainId, Message)` blob
    /// that a destination EVM handler will `abi.decode` and process.
    pub message: Vec<u8>,
}
