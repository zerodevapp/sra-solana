use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{approve_checked, ApproveChecked, Mint, Token, TokenAccount},
};
use sra_common::{
    constants::ACROSS_SPOKE_POOL_ID, seeds::SEED_ADAPTER_AUTHORITY,
    source_deposit::AcrossDepositParams, types::SraConfig,
};

use crate::events::AcrossDepositExecuted;
use crate::{utils::source_deposit::build_deposit_now_bundle, AcrossAdapterError};

fn triggered_nonce(sra_config: &SraConfig) -> Result<u64> {
    sra_config
        .deposit_nonce
        .checked_sub(1)
        .ok_or(error!(AcrossAdapterError::TriggeredNonceUnavailable))
}

#[derive(Accounts)]
pub struct ExecuteDepositAcross<'info> {
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// CHECK: Instructions sysvar used for paired-instruction attestation.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub sra_config: Box<Account<'info, SraConfig>>,
    /// CHECK: adapter authority PDA under this program; the handler derives the
    /// same address via `seeds::program = adapter_program.key()`.
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
    /// CHECK: validated by the Across program during the CPI.
    #[account(mut)]
    pub across_state: UncheckedAccount<'info>,
    /// Canonical ATA for `across_state` and `input_mint`.
    #[account(
        mut,
        associated_token::mint = input_mint,
        associated_token::authority = across_state,
    )]
    pub across_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: Across Anchor event-authority PDA derived from `__event_authority`.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = across_program.key())]
    pub across_event_authority: UncheckedAccount<'info>,
    /// CHECK: derived from the full deposit-now seed hash and checked in the handler.
    pub delegate: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Executes the adapter leg of a paired source deposit.
///
/// Most routing fields in `params` are only cross-checked here. The deposit
/// bundle is built from the verified prepare data plus the Across-specific
/// execution fields in `params`.
pub fn handler(ctx: Context<ExecuteDepositAcross>, params: AcrossDepositParams) -> Result<()> {
    // Reject unreasonable Across timing before any token movement.
    sra_common::source_deposit::validate_across_timing(
        params.fill_deadline_offset,
        params.exclusivity_period,
    )?;

    let prepare = sra_common::ix_attestation::verify_previous_trigger_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.input_mint.key(),
        ctx.program_id,
        &params,
    )?;

    let triggered_nonce = triggered_nonce(&ctx.accounts.sra_config)?;
    let deposit = build_deposit_now_bundle(
        &ctx.accounts.adapter_authority.key(),
        &ctx.accounts.sra_config,
        &ctx.accounts.input_mint.key(),
        &prepare,
        &params,
    )?;

    require_keys_eq!(
        ctx.accounts.delegate.key(),
        deposit.delegate.pda,
        AcrossAdapterError::InvalidDelegateAccount
    );

    let sra_config_key = ctx.accounts.sra_config.key();
    let adapter_authority_bump = [ctx.bumps.adapter_authority];
    let adapter_authority_seeds = sra_common::seeds::adapter_authority_signer_seeds(
        sra_config_key.as_ref(),
        &adapter_authority_bump,
    );
    approve_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            ApproveChecked {
                to: ctx.accounts.adapter_custody.to_account_info(),
                mint: ctx.accounts.input_mint.to_account_info(),
                delegate: ctx.accounts.delegate.to_account_info(),
                authority: ctx.accounts.adapter_authority.to_account_info(),
            },
            &[&adapter_authority_seeds],
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
        event_authority: ctx.accounts.across_event_authority.to_account_info(),
        program: ctx.accounts.across_program.to_account_info(),
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
        nonce: triggered_nonce,
        delegate: deposit.delegate.pda,
        across_state: ctx.accounts.across_state.key(),
        across_vault: ctx.accounts.across_vault.key(),
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        target_token: params.target_token,
        exclusive_relayer: params.exclusive_relayer,
        fill_deadline_offset: params.fill_deadline_offset,
        exclusivity_period: params.exclusivity_period,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sra_config() -> SraConfig {
        SraConfig {
            owner: Pubkey::new_unique(),
            final_handler: sra_common::ID,
            handler_data: sra_common::types::SourceHandlerData {
                address_book_root: Pubkey::new_unique(),
                target_chain_id: 42_161,
                recipient: [0x33; 32],
                target_token: [0xAB; 32],
                allowed_slippage_bps: 50,
                allowed_adapters: vec![crate::ID],
            },
            salt: [0x55; 32],
            initiator: Pubkey::new_unique(),
            active: true,
            deposit_nonce: 1,
            bump: 254,
        }
    }

    #[test]
    fn triggered_nonce_rejects_zero() {
        let mut sra_config = sample_sra_config();
        sra_config.deposit_nonce = 0;
        let err = triggered_nonce(&sra_config).expect_err("zero nonce must reject");
        assert_eq!(err, error!(AcrossAdapterError::TriggeredNonceUnavailable));
    }

    #[test]
    fn triggered_nonce_uses_pre_increment_value() {
        let mut sra_config = sample_sra_config();
        sra_config.deposit_nonce = 7;
        assert_eq!(triggered_nonce(&sra_config).unwrap(), 6);
    }
}
