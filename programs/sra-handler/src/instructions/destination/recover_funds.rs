//! Owner-gated destination fund recovery.
//!
//! Drains the route's execution vault to the route owner's canonical ATA (pinned
//! by authority + mint, so funds can only reach the pre-configured owner) and
//! advances the fill receipt to the terminal `Recovered` state; the replay-marker
//! receipt is never closed. The vault is one ATA per route shared by every fill,
//! so recovery drains the whole balance and advances only the referenced receipt
//! — fund-safe, since the sole authority is the owner and the sole destination is
//! their ATA.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::{
    errors::SraError,
    seeds::SEED_EXECUTION,
    types::{ExecutionStatus, FillReceipt, SolanaSraRoute},
};

use crate::events::FundsRecovered;

#[derive(Accounts)]
pub struct RecoverFunds<'info> {
    /// Route owner = recovery authority; must sign and equal `route.owner`.
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// Fill lifecycle marker, bound to this route. Never closed — recovery only
    /// advances its status, preserving replay protection.
    #[account(mut, has_one = route)]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    /// Bound to the route's committed mint; decimals read live for transfer_checked.
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    /// CHECK: PDA ["execution", route] — the execution-vault authority/signer.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    /// Payout target pinned to the owner's canonical ATA (authority + mint), so
    /// recovered funds can only reach the pre-configured owner account.
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub(crate) fn handler(ctx: Context<RecoverFunds>) -> Result<()> {
    // 1. Lifecycle gate: only a staged-but-unexecuted (`Finalized`) or failed
    //    (`ActionsFailed`) fill is recoverable; `ActionsSucceeded` and
    //    `Recovered` are terminal (the latter also blocks recovery replay).
    require!(
        matches!(
            ctx.accounts.fill_receipt.execution_status,
            ExecutionStatus::Finalized | ExecutionStatus::ActionsFailed
        ),
        SraError::InvalidExecutionStatus
    );

    // 2. Full-drain guard: never emit a zero-amount transfer or advance state
    //    with nothing to recover.
    let amount = ctx.accounts.execution_vault.amount;
    require!(amount > 0, SraError::NothingToRecover);

    // 3. Move the entire balance execution_vault -> owner ATA, signed by the
    //    execution-authority PDA (the sole signer for the vault).
    let route_key = ctx.accounts.route.key();
    let bump = [ctx.bumps.execution_authority];
    let seeds: &[&[u8]] = &[SEED_EXECUTION, route_key.as_ref(), bump.as_ref()];
    let signer_seeds: &[&[&[u8]]] = &[seeds];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.execution_vault.to_account_info(),
                mint: ctx.accounts.expected_mint.to_account_info(),
                to: ctx.accounts.owner_token_account.to_account_info(),
                authority: ctx.accounts.execution_authority.to_account_info(),
            },
            signer_seeds,
        ),
        amount,
        ctx.accounts.expected_mint.decimals,
    )?;

    // 4. Advance to the terminal `Recovered` state. The receipt is never closed,
    //    so the replay marker outlives recovery.
    ctx.accounts.fill_receipt.execution_status = ExecutionStatus::Recovered;

    emit!(FundsRecovered {
        route: route_key,
        owner: ctx.accounts.owner.key(),
        fill_id: ctx.accounts.fill_receipt.fill_id,
        amount,
    });

    Ok(())
}
