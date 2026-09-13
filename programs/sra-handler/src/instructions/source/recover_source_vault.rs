use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::{errors::SraError, seeds::SEED_SRA, types::SraConfig};

use crate::events::SourceVaultRecovered;

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

pub fn handler(ctx: Context<RecoverSourceVault>) -> Result<()> {
    let amount = ctx.accounts.sra_vault.amount;
    require!(amount > 0, SraError::NothingToRecover);

    let sra_config = &ctx.accounts.sra_config;
    let config_hash = sra_config
        .handler_data
        .config_hash(&sra_config.owner, &sra_config.final_handler);
    let bump = [sra_config.bump];
    let seeds: &[&[u8]] = &[
        SEED_SRA,
        sra_config.owner.as_ref(),
        config_hash.as_ref(),
        sra_config.salt.as_ref(),
        bump.as_ref(),
    ];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.sra_vault.to_account_info(),
                mint: ctx.accounts.source_mint.to_account_info(),
                to: ctx.accounts.owner_token_account.to_account_info(),
                authority: sra_config.to_account_info(),
            },
            &[seeds],
        ),
        amount,
        ctx.accounts.source_mint.decimals,
    )?;

    emit!(SourceVaultRecovered {
        sra_config: sra_config.key(),
        owner: ctx.accounts.owner.key(),
        source_mint: ctx.accounts.source_mint.key(),
        amount,
        deposit_nonce: sra_config.deposit_nonce,
    });
    Ok(())
}
