use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::{seeds::SEED_ADAPTER_AUTHORITY, types::SraConfig};

use crate::events::CustodyRecovered;
use crate::AcrossAdapterError;

/// Owner-gated recovery: drains a stranded `adapter_custody` refund back to the
/// owner's ATA, signed by the `adapter_authority` PDA (adapter-only).
#[derive(Accounts)]
pub struct RecoverStuckCustody<'info> {
    /// Pays the tx fee (gas-sponsorship parity with the deposit flow).
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// SRA owner = authority; must sign and match `sra_config.owner`.
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Box<Account<'info, SraConfig>>,
    /// CHECK: adapter authority PDA under this program; it signs the transfer
    /// out of `adapter_custody`. Address constrained via seeds + bump.
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = adapter_authority,
    )]
    pub adapter_custody: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<RecoverStuckCustody>) -> Result<()> {
    let amount = ctx.accounts.adapter_custody.amount;
    require!(amount > 0, AcrossAdapterError::NothingToRecover);
    let sra_config_key = ctx.accounts.sra_config.key();
    let adapter_authority_bump = [ctx.bumps.adapter_authority];
    let adapter_authority_seeds = sra_common::seeds::adapter_authority_signer_seeds(
        sra_config_key.as_ref(),
        &adapter_authority_bump,
    );
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.adapter_custody.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.owner_token_account.to_account_info(),
                authority: ctx.accounts.adapter_authority.to_account_info(),
            },
            &[&adapter_authority_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;
    emit!(CustodyRecovered {
        sra_config: ctx.accounts.sra_config.key(),
        mint: ctx.accounts.mint.key(),
        owner: ctx.accounts.owner.key(),
        amount,
    });
    Ok(())
}
