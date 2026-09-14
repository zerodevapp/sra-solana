//! Owner-gated destination fund recovery.
//!
//! Drains the route's execution vault to the route owner's canonical ATA (pinned
//! by authority + mint) and advances the fill receipt to the terminal `Recovered`
//! state.

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
    /// Recovery authority (route owner).
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// The fill lifecycle marker.
    #[account(mut, has_one = route)]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    /// Decimals read live for transfer_checked.
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    /// CHECK: execution-vault authority PDA; signs the transfer.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    /// Payout target: the owner's canonical ATA.
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub(crate) fn handler(ctx: Context<RecoverFunds>) -> Result<()> {
    require!(
        ctx.accounts.route.active_fill == Some(ctx.accounts.fill_receipt.fill_id),
        SraError::InvalidActiveFill
    );
    // Recoverable from Finalized (actions not yet run, or a run aborted before
    // marking failure — e.g. CU exhaustion) or ActionsFailed; the rest are terminal.
    require!(
        matches!(
            ctx.accounts.fill_receipt.execution_status,
            ExecutionStatus::Finalized | ExecutionStatus::ActionsFailed
        ),
        SraError::InvalidExecutionStatus
    );

    // Nothing to recover on an empty vault.
    let amount = ctx.accounts.execution_vault.amount;
    require!(amount > 0, SraError::NothingToRecover);

    // Drain the vault to the owner ATA, signed by the execution-authority PDA.
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

    ctx.accounts.fill_receipt.execution_status = ExecutionStatus::Recovered;
    ctx.accounts.route.active_fill = None;

    emit!(FundsRecovered {
        route: route_key,
        owner: ctx.accounts.owner.key(),
        fill_id: ctx.accounts.fill_receipt.fill_id,
        amount,
    });

    Ok(())
}
