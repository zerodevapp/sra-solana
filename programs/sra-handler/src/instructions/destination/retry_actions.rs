//! Owner-gated re-execution of a route's committed actions after an
//! `execute_actions` run left the receipt `ActionsFailed`. Re-runs the same
//! `action_config` through the shared `run_committed_actions`, so retry can never
//! diverge from first execution. Retryable indefinitely (no counter); the other
//! `ActionsFailed` escape is `recover_funds`.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use sra_common::errors::SraError;
use sra_common::seeds::{SEED_EXECUTION, SEED_FILL_RECEIPT};
use sra_common::types::{ExecutionStatus, FillReceipt, SolanaSraRoute, VerifiedCpiRuntime};

use super::execute_actions::{run_committed_actions, ActionAccounts, ActionOutcome};
use crate::events::{ActionsFailed, ActionsRetried};

#[derive(Accounts)]
#[instruction(fill_id: [u8; 32])]
pub struct RetryActions<'info> {
    /// Only the route owner may retry.
    pub owner: Signer<'info>,
    /// `has_one = owner`; not seed-bound (the `fill_receipt` PDA is seeded by `route`).
    #[account(has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// Replay/lifecycle marker: gates on `ActionsFailed`, advanced on a clean
    /// re-run, never closed.
    #[account(
        mut,
        seeds = [SEED_FILL_RECEIPT, route.key().as_ref(), &fill_id],
        bump = fill_receipt.bump,
    )]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    /// CHECK: PDA ["execution", route]; signs the payouts and vault->scratch move.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    /// CHECK: instructions sysvar; locates the `VerifiedCpi` signed price quote.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    // Per-action accounts ride in `remaining_accounts`, identical to `ExecuteActions`.
}

pub(crate) fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, RetryActions<'info>>,
    fill_id: [u8; 32],
    verified_runtimes: Vec<VerifiedCpiRuntime>,
) -> Result<()> {
    // Only an ActionsFailed receipt is retryable (Finalized -> execute_actions; terminal states can't re-run).
    require!(
        ctx.accounts.fill_receipt.execution_status == ExecutionStatus::ActionsFailed,
        SraError::InvalidExecutionStatus
    );
    // Pause gate (settlement path); stays owner-gated. recover_funds is NOT pause-gated.
    require!(!ctx.accounts.route.paused, SraError::RoutePaused);

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

    match run_committed_actions(&accounts, fill_id, &verified_runtimes)? {
        ActionOutcome::Succeeded => {
            // The re-run applied. ActionsRetried is the retry-path success event
            // (the counterpart of execute_actions' ActionsExecuted), not a re-attempt.
            ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsSucceeded;
            emit!(ActionsRetried { route, fill_id });
        }
        ActionOutcome::SoftFailed => {
            // Already ActionsFailed; leave it and re-emit so the attempt is observable.
            emit!(ActionsFailed { route, fill_id });
        }
    }
    Ok(())
}
