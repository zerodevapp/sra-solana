use anchor_lang::prelude::*;

use crate::constants::MAX_ADAPTERS;

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
    /// keccak256(borsh(action_config)). The action list is also folded into the
    /// route's config_hash, but stored only as this digest; callers re-supply the
    /// list as ix data and verify it against this hash, keeping the account small.
    pub action_config_hash: [u8; 32],
    // Immutable (NOT in config_hash)
    pub salt: [u8; 32],
    // Mutable
    #[max_len(MAX_ADAPTERS)]
    pub allowed_adapters: Vec<Pubkey>,
    pub paused: bool,
    pub bump: u8,
}

/// Inputs to `create_route`. Identity fields are folded into `route_config_hash`
/// (the route PDA); `allowed_adapters` is the only later-mutable policy.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CreateRouteParams {
    pub source_chain_id: u64,
    pub expected_mint: Pubkey,
    pub app_id: [u8; 32],
    pub action_config: Vec<SolanaAction>,
    pub salt: [u8; 32],
    pub allowed_adapters: Vec<Pubkey>,
}

/// Inputs to `manage_route`. Only the route's mutable policy can change here —
/// `pause` toggles the route, and the adapter allowlist is edited via `add`/
/// `remove`. Identity fields (owner, mint, chain, app_id, actions, salt) are in
/// `route_config_hash` and are structurally immutable.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ManageRouteParams {
    /// `Some(true)` pauses, `Some(false)` unpauses, `None` leaves it unchanged.
    pub pause: Option<bool>,
    pub add_adapters: Vec<Pubkey>,
    pub remove_adapters: Vec<Pubkey>,
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
        // + 32 action_config_hash + 32 salt
        // + 4 + (32 * 8) allowed_adapters + 1 paused + 1 bump
        // = 32 + 8 + 32 + 32 + 32 + 32 + 260 + 1 + 1 = 430
        assert_eq!(SolanaSraRoute::INIT_SPACE, 430);
    }

    #[test]
    fn solana_sra_route_stores_action_config_hash_not_inline_actions() {
        // Commit-by-hash: the route carries a 32-byte digest of the action list,
        // never the Vec<SolanaAction> itself.
        let route = SolanaSraRoute {
            owner: Pubkey::new_unique(),
            source_chain_id: 1,
            expected_mint: Pubkey::new_unique(),
            app_id: [0u8; 32],
            action_config_hash: [7u8; 32],
            salt: [0u8; 32],
            allowed_adapters: vec![],
            paused: false,
            bump: 255,
        };
        assert_eq!(route.action_config_hash, [7u8; 32]);
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
