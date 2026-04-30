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

use crate::events::AcrossDepositExecuted;
use crate::{utils::source_deposit::build_deposit_now_bundle, AcrossAdapterError};

/// Recover the pre-bump nonce written by the paired `validate_and_prepare`.
///
/// In the paired flow IX 1 always increments `mint_nonce.nonce` before IX 2
/// reads it, so this guard is defense-in-depth: an unincremented account
/// (zero nonce, untouched by IX 1) cannot produce a valid Across deposit
/// extra-envelope. Coverage lives in the helper-level unit tests below;
/// the branch is structurally unreachable end-to-end because attestation
/// would fail first if IX 1 were missing.
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
    /// CHECK: PDA `["adapter_authority", sra_config]` under this adapter program
    /// (`seeds::program` defaults to `ctx.program_id`). The handler's
    /// `validate_and_prepare` derives the symmetric PDA explicitly via
    /// `seeds::program = adapter_program.key()`; both resolve identically.
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
    /// CHECK: SpokePool state PDA — owned and validated by `across_program`;
    /// passed through to the `deposit_now` CPI.
    #[account(mut)]
    pub across_state: UncheckedAccount<'info>,
    /// Pin the destination of the `deposit_now` token movement to the canonical
    /// ATA(`across_state`, `input_mint`).
    #[account(
        mut,
        associated_token::mint = input_mint,
        associated_token::authority = across_state,
    )]
    pub across_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: derived from the full deposit-now seed hash and checked in the handler.
    pub delegate: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Adapter-side ix of the source 2-IX flow.
///
/// `params.input_amount`, `params.output_amount`, `params.target_token`, and
/// `params.action_idxs` exist solely to be cross-checked against the paired
/// handler IX's `PrepareParams`. After attestation succeeds, all routing-bearing
/// values flow from the verified handler `prepare` — only the Across-specific
/// fields (`exclusive_relayer`, `fill_deadline_offset`, `exclusivity_period`)
/// from `params` are consumed downstream.
pub fn handler(ctx: Context<ExecuteDepositAcross>, params: AcrossDepositParams) -> Result<()> {
    let prepare = crate::utils::ix_attestation::verify_previous_handler_ix(
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

    emit!(AcrossDepositExecuted {
        sra_config: ctx.accounts.sra_config.key(),
        input_mint: ctx.accounts.input_mint.key(),
        nonce: prepared_nonce,
        delegate: deposit.delegate.pda,
        across_state: ctx.accounts.across_state.key(),
        across_vault: ctx.accounts.across_vault.key(),
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        target_token: params.target_token,
        action_idxs: params.action_idxs,
        exclusive_relayer: params.exclusive_relayer,
        fill_deadline_offset: params.fill_deadline_offset,
        exclusivity_period: params.exclusivity_period,
    });

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
