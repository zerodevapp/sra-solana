use anchor_lang::prelude::*;

use crate::constants::{MAX_ACTION_IDXS, MAX_ADAPTERS};

/// A pre-registered action to execute on the destination chain after fill finalization.
/// v1: only SplTransfer is implemented.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum SolanaAction {
    /// Transfer a percentage of execution vault balance to a recipient.
    SplTransfer {
        recipient: Pubkey,
        /// Basis points of the execution vault balance to transfer (e.g., 10000 = 100%).
        amount_bps: u16,
    },
}

/// Deterministic destination address for EVM->Solana fills.
/// PDA seeds: ["sra_route", owner, config_hash, salt]
/// config_hash = keccak256(borsh(owner, source_chain_id, expected_mint, app_id, action_config))
#[account]
#[derive(InitSpace)]
pub struct SolanaSraRoute {
    // Immutable (in config_hash)
    pub owner: Pubkey,
    pub source_chain_id: u64,
    pub expected_mint: Pubkey,
    pub app_id: [u8; 32],
    #[max_len(MAX_ACTION_IDXS)]
    pub action_config: Vec<SolanaAction>,
    // Immutable (NOT in config_hash)
    pub salt: [u8; 32],
    /// Snapshot of `expected_mint`'s decimals, captured from the validated `Mint`
    /// account at route creation (never from caller args). Kept out of the
    /// config_hash because it is derivable from `expected_mint`; stored so
    /// downstream cross-decimal math and indexers need not re-read the mint.
    pub expected_decimals: u8,
    // Mutable
    #[max_len(MAX_ADAPTERS)]
    pub allowed_adapters: Vec<Pubkey>,
    pub fee_bps: u16,
    pub paused: bool,
    pub bump: u8,
}

/// Execution status state machine for fill lifecycle.
/// Borsh-serialized as a single u8 discriminant (no variant data). Size = 1 byte.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum ExecutionStatus {
    Finalized,
    ActionsSucceeded,
    ActionsFailed,
    Recovered,
}

/// Replay protection + execution lifecycle.
/// PDA seeds: ["fill_receipt", route, fill_id]
#[account]
#[derive(InitSpace)]
pub struct FillReceipt {
    pub route: Pubkey,
    pub fill_id: [u8; 32],
    pub amount: u64,
    pub expected_amount: u64,
    pub execution_status: ExecutionStatus,
    pub finalized_at: i64,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solana_action_init_space() {
        // enum disc (1) + Pubkey (32) + u16 (2) = 35
        assert_eq!(SolanaAction::INIT_SPACE, 35);
    }

    #[test]
    fn solana_sra_route_init_space() {
        // 32 owner + 8 source_chain_id + 32 expected_mint + 32 app_id
        // + 4 + (35 * 4) action_config + 32 salt + 1 expected_decimals
        // + 4 + (32 * 8) allowed_adapters + 2 fee_bps + 1 paused + 1 bump
        // = 32 + 8 + 32 + 32 + 144 + 32 + 1 + 260 + 2 + 1 + 1 = 545
        assert_eq!(SolanaSraRoute::INIT_SPACE, 545);
    }

    #[test]
    fn fill_receipt_init_space() {
        // 32 route + 32 fill_id + 8 amount + 8 expected_amount + 1 execution_status + 8 finalized_at + 1 bump = 90
        assert_eq!(FillReceipt::INIT_SPACE, 90);
    }

    #[test]
    fn execution_status_variants() {
        assert_ne!(
            ExecutionStatus::Finalized,
            ExecutionStatus::ActionsSucceeded
        );
        assert_ne!(ExecutionStatus::ActionsFailed, ExecutionStatus::Recovered);
    }
}
