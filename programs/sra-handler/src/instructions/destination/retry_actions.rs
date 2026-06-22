//! Owner-gated re-execution of a route's committed actions.
//!
//! When `execute_actions` hits a recoverable per-account failure it persists
//! `ActionsFailed` and leaves the funds staged. `retry_actions` lets the route
//! owner re-run the same committed `action_config` through the same shared engine
//! (`run_committed_actions`) once the underlying problem is fixed out-of-band
//! (the recipient ATA created, a fresh quote provided). It runs indefinitely —
//! there is no retry counter, matching the model where a failed target action
//! leaves the funds usable with no attempt limit. The only other escape from
//! `ActionsFailed` is `recover_funds` (drain to the owner).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use sra_common::errors::SraError;
use sra_common::seeds::{SEED_EXECUTION, SEED_FILL_RECEIPT};
use sra_common::types::{
    ExecutionStatus, FillReceipt, SolanaAction, SolanaSraRoute, VerifiedCpiRuntime,
};

use super::execute_actions::{run_committed_actions, ActionAccounts, ActionOutcome};
use crate::events::{ActionsFailed, ActionsRetried};

#[derive(Accounts)]
#[instruction(fill_id: [u8; 32])]
pub struct RetryActions<'info> {
    /// Only the route owner may retry — the recovery counterpart of `recover_funds`.
    pub owner: Signer<'info>,
    /// Bound to `owner` by `has_one`. Not seed-bound (same rationale as
    /// `ExecuteActions`): the `fill_receipt` PDA below is seeded by `route.key()`.
    #[account(has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// The replay/lifecycle marker; bound to `route` + `fill_id` by its seeds. Read
    /// for the `ActionsFailed` precondition and advanced to `ActionsSucceeded` on a
    /// clean re-run. Never closed (closing it would re-enable replay of the fill).
    #[account(
        mut,
        seeds = [SEED_FILL_RECEIPT, route.key().as_ref(), &fill_id],
        bump = fill_receipt.bump,
    )]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    /// CHECK: PDA ["execution", route] — the execution-vault authority; validated
    /// by seeds and signs the `SplTransfer` payouts and the vault->scratch move.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    /// CHECK: instructions sysvar, pinned by address; read to locate the signed
    /// price quote backing a `VerifiedCpi` action's `min_out`.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    // Per-action accounts ride in `remaining_accounts`, identical to `ExecuteActions`.
}

pub(crate) fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, RetryActions<'info>>,
    fill_id: [u8; 32],
    action_config: Vec<SolanaAction>,
    verified_runtimes: Vec<VerifiedCpiRuntime>,
) -> Result<()> {
    // Retry only operates on a previously-failed run. A Finalized receipt belongs
    // to execute_actions, and the terminal states cannot be re-run.
    require!(
        ctx.accounts.fill_receipt.execution_status == ExecutionStatus::ActionsFailed,
        SraError::InvalidExecutionStatus
    );

    let route = ctx.accounts.route.key();
    let accounts = ActionAccounts {
        route: &ctx.accounts.route,
        expected_mint: &ctx.accounts.expected_mint,
        execution_authority: &ctx.accounts.execution_authority,
        execution_vault: &ctx.accounts.execution_vault,
        token_program: &ctx.accounts.token_program,
        instructions_sysvar: &ctx.accounts.instructions_sysvar,
        remaining_accounts: ctx.remaining_accounts,
        execution_authority_bump: ctx.bumps.execution_authority,
    };

    match run_committed_actions(&accounts, fill_id, &action_config, &verified_runtimes)? {
        ActionOutcome::Succeeded => {
            ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsSucceeded;
            emit!(ActionsRetried { route, fill_id });
        }
        ActionOutcome::SoftFailed => {
            // Still failing: the status is already ActionsFailed (the precondition),
            // so leave it; re-emit the marker so the attempt is observable. Funds
            // stay staged and the route can be retried again later.
            emit!(ActionsFailed { route, fill_id });
        }
    }
    Ok(())
}
