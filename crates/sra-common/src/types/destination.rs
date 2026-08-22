use anchor_lang::prelude::*;

use crate::constants::{MAX_ACTION_IDXS, MAX_ADAPTERS, MAX_EXECUTORS};
use crate::types::actions::{AccountTemplate, MinOut};

/// A pre-registered action to execute on the destination chain after fill
/// finalization. v1: `SplTransfer` (static, exact-match accounts) and
/// `VerifiedCpi` (dynamic accounts, outcome-verified).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum SolanaAction {
    /// Transfer a percentage of the finalized fill amount to a recipient.
    SplTransfer {
        recipient: Pubkey,
        /// Basis points of the finalized fill amount to transfer (e.g., 10000 = 100%).
        amount_bps: u16,
    },
    /// Spend a fraction of the finalized fill amount through a pinned program,
    /// verifying the post-fee output by balance-diff against `min_out`. Routing
    /// accounts are relayer-supplied (`Free` slots); the value-bearing slots are
    /// pinned by `account_template`.
    VerifiedCpi {
        /// Pinned program — checked loader-owned-executable + exact pubkey at execution.
        program_id: Pubkey,
        /// Token spent from the vault (usually `expected_mint`).
        input_mint: Pubkey,
        /// Basis points of the finalized fill amount moved into the scratch source.
        input_bps: u16,
        account_template: AccountTemplate,
        /// Fungible receipt the engine balance-diffs (swap output, vault share, LST, …).
        output_mint: Pubkey,
        min_out: MinOut,
        /// Opt-in for issuer-controlled or transfer-restricted Token-2022 output mints.
        allow_permissioned_output: bool,
    },
}

/// Deterministic destination address for EVM->Solana fills.
/// PDA seeds: ["sra_route", owner, config_hash, salt]
/// config_hash = keccak256(borsh(owner, source_chain_id, expected_mint, action_config))
#[account]
#[derive(InitSpace)]
pub struct SolanaSraRoute {
    // Immutable (in config_hash)
    pub owner: Pubkey,
    pub source_chain_id: u64,
    pub expected_mint: Pubkey,
    /// The committed action list, stored typed so later instructions read it
    /// directly instead of re-verifying a relayer-supplied copy. Also folded into
    /// the route's config_hash (the PDA seed), so the address inherently commits
    /// to it.
    #[max_len(MAX_ACTION_IDXS)]
    pub action_config: Vec<SolanaAction>,
    // Immutable (NOT in config_hash)
    pub salt: [u8; 32],
    // Mutable
    #[max_len(MAX_ADAPTERS)]
    pub allowed_adapters: Vec<Pubkey>,
    /// Owner-managed; not in `route_config_hash` (mutable, like `allowed_adapters`).
    /// Empty = no caller can `execute_actions`.
    #[max_len(MAX_EXECUTORS)]
    pub allowed_executors: Vec<Pubkey>,
    pub paused: bool,
    pub bump: u8,
}

/// Inputs to `create_route`. Identity fields are folded into `route_config_hash`
/// (the route PDA); `allowed_adapters` is the only later-mutable policy.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CreateRouteParams {
    pub source_chain_id: u64,
    pub expected_mint: Pubkey,
    pub action_config: Vec<SolanaAction>,
    pub salt: [u8; 32],
    pub allowed_adapters: Vec<Pubkey>,
    /// Executor allowlist; empty allowed.
    pub allowed_executors: Vec<Pubkey>,
}

/// Inputs to `manage_route` — the route's only mutable policy (identity fields
/// are immutable, baked into `route_config_hash`). Each field is independently
/// optional: `None` leaves that policy unchanged.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ManageRouteParams {
    /// `Some(true)` pauses, `Some(false)` unpauses, `None` leaves it unchanged.
    pub pause: Option<bool>,
    /// `Some` replaces the whole adapter allowlist (EVM `_installAdapters`
    /// parity); `Some(vec![])` clears it; `None` leaves it unchanged.
    pub adapters: Option<Vec<Pubkey>>,
    /// Whole-list replace, mirroring `adapters`; `Some(vec![])` clears (freezes execution).
    pub executors: Option<Vec<Pubkey>>,
}

/// Per-`VerifiedCpi`-action runtime data the relayer supplies to `execute_actions`,
/// one entry per `VerifiedCpi` action in action order. Carries the inner CPI's
/// instruction data plus the signed-quote fields (`min_amount_out`, `expiry`); the
/// quote's signature rides in a separate ed25519 instruction in the same
/// transaction. `min_amount_out`/`expiry` are bound into the quote message and
/// verified against the route-committed quoter, so a relayer cannot inflate them.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct VerifiedCpiRuntime {
    /// The inner program's instruction data (relayer-built swap/deposit args).
    pub inner_data: Vec<u8>,
    /// The signed quote's minimum acceptable output (the balance-diff floor).
    pub min_amount_out: u64,
    /// The signed quote's unix expiry.
    pub expiry: i64,
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
    use crate::types::{FixedRole, SlotRole};

    #[test]
    fn solana_action_init_space_covers_verified_cpi() {
        // InitSpace reserves the larger variant. VerifiedCpi = 32 program_id
        // + 32 input_mint + 2 input_bps + 68 account_template + 32 output_mint
        // + 35 min_out + 1 allow_permissioned_output = 202; enum disc (1) + 202.
        assert_eq!(SolanaAction::INIT_SPACE, 203);
    }

    fn sample_verified_cpi() -> SolanaAction {
        SolanaAction::VerifiedCpi {
            program_id: Pubkey::new_unique(),
            input_mint: Pubkey::new_unique(),
            input_bps: 10_000,
            account_template: AccountTemplate {
                slots: vec![
                    SlotRole::Fixed(FixedRole::SigningAuthority),
                    SlotRole::Fixed(FixedRole::Source),
                    SlotRole::Fixed(FixedRole::Destination),
                    SlotRole::Fixed(FixedRole::OutputMint),
                    SlotRole::Free,
                ],
            },
            output_mint: Pubkey::new_unique(),
            min_out: MinOut::Oracle {
                feed: Pubkey::new_unique(),
                max_slippage_bps: 50,
            },
            allow_permissioned_output: true,
        }
    }

    #[test]
    fn solana_action_borsh_round_trip_spl_transfer() {
        let action = SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 7_500,
        };
        let bytes = action.try_to_vec().unwrap();
        assert_eq!(SolanaAction::try_from_slice(&bytes).unwrap(), action);
    }

    #[test]
    fn solana_action_borsh_round_trip_verified_cpi() {
        let action = sample_verified_cpi();
        let bytes = action.try_to_vec().unwrap();
        assert_eq!(SolanaAction::try_from_slice(&bytes).unwrap(), action);
    }

    #[test]
    fn solana_sra_route_init_space() {
        // 32 owner + 8 source_chain_id + 32 expected_mint
        // + 4 + (4 * 203) action_config + 32 salt
        // + 4 + (32 * 8) allowed_adapters
        // + 4 + (32 * 8) allowed_executors + 1 paused + 1 bump
        // = 32 + 8 + 32 + (4 + 812) + 32 + 260 + 260 + 1 + 1 = 1442
        assert_eq!(SolanaSraRoute::INIT_SPACE, 1442);
    }

    #[test]
    fn solana_sra_route_stores_typed_action_config() {
        // The route stores the committed action list itself, so execute_actions
        // reads it directly instead of re-verifying a relayer-supplied copy.
        let action = SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 5_000,
        };
        let route = SolanaSraRoute {
            owner: Pubkey::new_unique(),
            source_chain_id: 1,
            expected_mint: Pubkey::new_unique(),
            action_config: vec![action.clone()],
            salt: [0u8; 32],
            allowed_adapters: vec![],
            allowed_executors: vec![],
            paused: false,
            bump: 255,
        };
        assert_eq!(route.action_config, vec![action]);
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
