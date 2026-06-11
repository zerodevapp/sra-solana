use anchor_lang::prelude::*;
use sra_common::{
    constants::MAX_FEE_BPS,
    errors::SraError,
    seeds::{SEED_APP_FEE, SEED_FEE_CONFIG},
    types::{AppFeeEntry, FeeConfig},
};

use crate::events::{AppFeeRegistered, FeeConfigInitialized};

#[derive(Accounts)]
pub struct InitializeFeeConfig<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + FeeConfig::INIT_SPACE,
        seeds = [SEED_FEE_CONFIG],
        bump,
    )]
    pub fee_config: Account<'info, FeeConfig>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn initialize_fee_config_handler(
    ctx: Context<InitializeFeeConfig>,
    protocol_fee_bps: u16,
    protocol_fee_recipient: Pubkey,
) -> Result<()> {
    // Bound at write time so consumers can rely on protocol + app <= MAX_FEE_BPS.
    require!(protocol_fee_bps <= MAX_FEE_BPS, SraError::FeeTooHigh);

    let fee_config = &mut ctx.accounts.fee_config;
    fee_config.admin = ctx.accounts.admin.key();
    fee_config.protocol_fee_bps = protocol_fee_bps;
    fee_config.protocol_fee_recipient = protocol_fee_recipient;
    fee_config.bump = ctx.bumps.fee_config;

    emit!(FeeConfigInitialized {
        fee_config: fee_config.key(),
        admin: fee_config.admin,
        protocol_fee_bps: fee_config.protocol_fee_bps,
        protocol_fee_recipient: fee_config.protocol_fee_recipient,
    });

    Ok(())
}

/// Admin-gated upsert of a per-app fee; the fill path MUST key `app_fee_entry`
/// off the committed `route.app_id` (never caller input) to prevent spoofing.
#[derive(Accounts)]
#[instruction(app_id: [u8; 32])]
pub struct RegisterAppFee<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub admin: Signer<'info>,
    #[account(
        seeds = [SEED_FEE_CONFIG],
        bump = fee_config.bump,
        has_one = admin,
    )]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + AppFeeEntry::INIT_SPACE,
        seeds = [SEED_APP_FEE, app_id.as_ref()],
        bump,
    )]
    pub app_fee_entry: Account<'info, AppFeeEntry>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn register_app_fee_handler(
    ctx: Context<RegisterAppFee>,
    app_id: [u8; 32],
    app_fee_bps: u16,
    app_fee_recipient: Pubkey,
) -> Result<()> {
    // app_id 0 is the "no app fee" sentinel; not registrable.
    require!(app_id != [0u8; 32], SraError::InvalidAppId);

    // u32 widening keeps the bps sum overflow-free.
    let combined = ctx.accounts.fee_config.protocol_fee_bps as u32 + app_fee_bps as u32;
    require!(combined <= MAX_FEE_BPS as u32, SraError::FeeTooHigh);

    let app_fee_entry = &mut ctx.accounts.app_fee_entry;
    app_fee_entry.app_id = app_id;
    app_fee_entry.app_fee_bps = app_fee_bps;
    app_fee_entry.app_fee_recipient = app_fee_recipient;
    app_fee_entry.bump = ctx.bumps.app_fee_entry;

    emit!(AppFeeRegistered {
        app_fee_entry: app_fee_entry.key(),
        app_id: app_fee_entry.app_id,
        app_fee_bps: app_fee_entry.app_fee_bps,
        app_fee_recipient: app_fee_entry.app_fee_recipient,
    });

    Ok(())
}
