use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use sra_common::{errors::SraError, types::SraConfig};

use crate::events::SourceVaultRecovered;
use crate::utils::transfer_from_sra_vault;

#[derive(Accounts)]
pub struct RecoverSourceVault<'info> {
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Box<Account<'info, SraConfig>>,
    pub source_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Box<Account<'info, TokenAccount>>,
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub(crate) fn handler(ctx: Context<RecoverSourceVault>) -> Result<()> {
    let amount = ctx.accounts.sra_vault.amount;
    require!(amount > 0, SraError::NothingToRecover);
    transfer_from_sra_vault(
        &ctx.accounts.sra_config,
        &ctx.accounts.sra_vault,
        &ctx.accounts.source_mint,
        &ctx.accounts.owner_token_account,
        &ctx.accounts.token_program,
        amount,
    )?;

    emit!(SourceVaultRecovered {
        sra_config: ctx.accounts.sra_config.key(),
        owner: ctx.accounts.owner.key(),
        source_mint: ctx.accounts.source_mint.key(),
        amount,
        deposit_nonce: ctx.accounts.sra_config.deposit_nonce,
    });
    Ok(())
}
