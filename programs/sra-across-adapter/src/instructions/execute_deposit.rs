use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{approve_checked, ApproveChecked, Mint, Token, TokenAccount},
};
use sra_common::{
    constants::ACROSS_SPOKE_POOL_ID,
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_MINT_NONCE},
    source_deposit::AcrossDepositParams,
    types::{MintNonce, SraConfig},
};

use crate::{utils::build_deposit_now_bundle, AcrossAdapterError};

fn prepared_nonce(mint_nonce: &MintNonce) -> Result<u64> {
    mint_nonce
        .nonce
        .checked_sub(1)
        .ok_or(error!(AcrossAdapterError::PreparedNonceUnavailable))
}

#[derive(Accounts)]
pub struct ExecuteDepositAcross<'info> {
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// CHECK: pinned to the canonical Instructions sysvar for neighboring-IX attestation.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub sra_config: Box<Account<'info, SraConfig>>,
    #[account(
        seeds = [SEED_MINT_NONCE, sra_config.key().as_ref(), input_mint.key().as_ref()],
        bump = mint_nonce.bump,
        seeds::program = sra_common::ID,
    )]
    pub mint_nonce: Box<Account<'info, MintNonce>>,
    /// CHECK: paired with handler-derived PDA; deeper checks land with full implementation.
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = input_mint,
        associated_token::authority = adapter_authority,
    )]
    pub adapter_custody: Box<Account<'info, TokenAccount>>,
    pub input_mint: Box<Account<'info, Mint>>,
    /// CHECK: canonical Across SpokePool program.
    #[account(address = ACROSS_SPOKE_POOL_ID)]
    pub across_program: UncheckedAccount<'info>,
    /// CHECK: Placeholder until full Across CPI wiring lands.
    #[account(mut)]
    pub across_state: UncheckedAccount<'info>,
    #[account(mut, constraint = across_vault.mint == input_mint.key())]
    pub across_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: derived from the full deposit-now seed hash and checked in the handler.
    pub delegate: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ExecuteDepositAcross>, params: AcrossDepositParams) -> Result<()> {
    let prepare = crate::utils::verify_previous_handler_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.input_mint.key(),
        ctx.program_id,
        &params,
    )?;

    let prepared_nonce = prepared_nonce(&ctx.accounts.mint_nonce)?;
    let deposit = build_deposit_now_bundle(
        &ctx.accounts.adapter_authority.key(),
        &ctx.accounts.sra_config,
        &ctx.accounts.input_mint.key(),
        &prepare,
        &params,
        prepared_nonce,
    )?;

    require_keys_eq!(
        ctx.accounts.delegate.key(),
        deposit.delegate.pda,
        AcrossAdapterError::InvalidDelegateAccount
    );

    let sra_config_key = ctx.accounts.sra_config.key();
    let adapter_authority_seeds: &[&[u8]] = &[
        SEED_ADAPTER_AUTHORITY,
        sra_config_key.as_ref(),
        &[ctx.bumps.adapter_authority],
    ];
    approve_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            ApproveChecked {
                to: ctx.accounts.adapter_custody.to_account_info(),
                mint: ctx.accounts.input_mint.to_account_info(),
                delegate: ctx.accounts.delegate.to_account_info(),
                authority: ctx.accounts.adapter_authority.to_account_info(),
            },
            &[adapter_authority_seeds],
        ),
        deposit.params.input_amount,
        ctx.accounts.input_mint.decimals,
    )?;

    let cpi_accounts = crate::utils::svm_spoke::cpi::accounts::DepositNow {
        signer: ctx.accounts.fee_payer.to_account_info(),
        state: ctx.accounts.across_state.to_account_info(),
        delegate: ctx.accounts.delegate.to_account_info(),
        depositor_token_account: ctx.accounts.adapter_custody.to_account_info(),
        vault: ctx.accounts.across_vault.to_account_info(),
        mint: ctx.accounts.input_mint.to_account_info(),
        token_program: ctx.accounts.token_program.to_account_info(),
        associated_token_program: ctx.accounts.associated_token_program.to_account_info(),
        system_program: ctx.accounts.system_program.to_account_info(),
    };
    crate::utils::svm_spoke::cpi::deposit_now(
        CpiContext::new(ctx.accounts.across_program.to_account_info(), cpi_accounts),
        deposit.params.depositor,
        deposit.params.recipient,
        deposit.params.input_token,
        deposit.params.output_token,
        deposit.params.input_amount,
        deposit.params.output_amount,
        deposit.params.destination_chain_id,
        deposit.params.exclusive_relayer,
        deposit.params.fill_deadline_offset,
        deposit.params.exclusivity_period,
        deposit.params.message,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_nonce_rejects_zero() {
        let mint_nonce = MintNonce { nonce: 0, bump: 1 };
        let err = prepared_nonce(&mint_nonce).expect_err("zero nonce must reject");
        assert_eq!(err, error!(AcrossAdapterError::PreparedNonceUnavailable));
    }

    #[test]
    fn prepared_nonce_uses_pre_increment_value() {
        let mint_nonce = MintNonce { nonce: 7, bump: 1 };
        assert_eq!(prepared_nonce(&mint_nonce).unwrap(), 6);
    }
}
