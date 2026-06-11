//! Fee admin (EVM BaseHandler parity): a per-app tiered PROTOCOL fee paid to one
//! global `fee_recipient`. Apps are fee payers, never payees. PDA existence is
//! the EVM `isSet` flag — `remove_*` closes the PDA. Will be consumed by the
//! source deposit path (the resolver lands separately); the destination
//! charges nothing.

use anchor_lang::prelude::*;
use sra_common::{
    constants::MAX_FEE_BPS,
    errors::SraError,
    seeds::{SEED_ASSET_FEE_TIER, SEED_FEE_CONFIG, SEED_PROTOCOL_FEE},
    types::{AssetFeeTier, FeeConfig, ProtocolFeeConfig},
};

use crate::events::{
    AdminTransferred, AssetFeeTierRemoved, AssetFeeTierUpdated, FeeConfigInitialized,
    FeeRecipientUpdated, PendingAdminSet, ProtocolFeeRemoved, ProtocolFeeUpdated,
};

// ---------------------------------------------------------------------------
// initialize_fee_config
// ---------------------------------------------------------------------------

/// First-caller-wins singleton (EVM sets `admin` atomically in the
/// constructor; Solana cannot). Deployment policy: initialize in the same
/// bundle as the deploy and VERIFY `fee_config.admin` before burning the
/// upgrade authority — a squatted singleton is unrecoverable afterwards.
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

/// `fee_recipient = Pubkey::default()` leaves fees disabled (EVM address(0)).
pub(crate) fn initialize_fee_config_handler(
    ctx: Context<InitializeFeeConfig>,
    fee_recipient: Pubkey,
) -> Result<()> {
    let fee_config = &mut ctx.accounts.fee_config;
    fee_config.admin = ctx.accounts.admin.key();
    fee_config.pending_admin = Pubkey::default();
    fee_config.fee_recipient = fee_recipient;
    fee_config.bump = ctx.bumps.fee_config;

    emit!(FeeConfigInitialized {
        fee_config: fee_config.key(),
        admin: fee_config.admin,
        fee_recipient: fee_config.fee_recipient,
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// set_fee_recipient
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct SetFeeRecipient<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
}

/// `Pubkey::default()` disables all fees globally (EVM setFeeRecipient(0)).
pub(crate) fn set_fee_recipient_handler(
    ctx: Context<SetFeeRecipient>,
    new_recipient: Pubkey,
) -> Result<()> {
    let fee_config = &mut ctx.accounts.fee_config;
    let old_fee_recipient = fee_config.fee_recipient;
    fee_config.fee_recipient = new_recipient;

    emit!(FeeRecipientUpdated {
        old_fee_recipient,
        new_fee_recipient: fee_config.fee_recipient,
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// set_protocol_fee / remove_protocol_fee
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(app_id: [u8; 32])]
pub struct SetProtocolFee<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + ProtocolFeeConfig::INIT_SPACE,
        seeds = [SEED_PROTOCOL_FEE, app_id.as_ref()],
        bump,
    )]
    pub protocol_fee_config: Account<'info, ProtocolFeeConfig>,
    pub system_program: Program<'info, System>,
}

/// Upsert (EVM setProtocolFee + setProtocolFeeOverridable). Any `app_id` is
/// registrable, including zero — `protocolFeeConfig[bytes32(0)]` prices
/// app-less deposits on EVM.
pub(crate) fn set_protocol_fee_handler(
    ctx: Context<SetProtocolFee>,
    app_id: [u8; 32],
    default_fee_bps: u16,
    allow_override: bool,
) -> Result<()> {
    require!(default_fee_bps <= MAX_FEE_BPS, SraError::FeeTooHigh);

    let protocol_fee_config = &mut ctx.accounts.protocol_fee_config;
    protocol_fee_config.app_id = app_id;
    protocol_fee_config.default_fee_bps = default_fee_bps;
    protocol_fee_config.allow_override = allow_override;
    protocol_fee_config.bump = ctx.bumps.protocol_fee_config;

    emit!(ProtocolFeeUpdated {
        app_id: protocol_fee_config.app_id,
        default_fee_bps: protocol_fee_config.default_fee_bps,
        allow_override: protocol_fee_config.allow_override,
    });

    Ok(())
}

#[derive(Accounts)]
#[instruction(app_id: [u8; 32])]
pub struct RemoveProtocolFee<'info> {
    /// Receives the closed PDA's rent.
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        mut,
        close = admin,
        seeds = [SEED_PROTOCOL_FEE, app_id.as_ref()],
        bump = protocol_fee_config.bump,
    )]
    pub protocol_fee_config: Account<'info, ProtocolFeeConfig>,
}

/// Closing the PDA is the EVM `removeProtocolFee` — the app's fee falls back
/// to "not configured" (= 0). Existing `AssetFeeTier` PDAs survive (inert
/// while no config exists, revived by re-registration — same as EVM's
/// dangling `assetFeeTiers` entries).
pub(crate) fn remove_protocol_fee_handler(
    ctx: Context<RemoveProtocolFee>,
    _app_id: [u8; 32],
) -> Result<()> {
    emit!(ProtocolFeeRemoved {
        app_id: ctx.accounts.protocol_fee_config.app_id,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// set_asset_fee_tier / remove_asset_fee_tier
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(app_id: [u8; 32], mint: Pubkey)]
pub struct SetAssetFeeTier<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    /// Must exist (EVM ProtocolFeeNotSet): a tier refines an app's configured
    /// fee; it cannot create one.
    #[account(seeds = [SEED_PROTOCOL_FEE, app_id.as_ref()], bump = protocol_fee_config.bump)]
    pub protocol_fee_config: Account<'info, ProtocolFeeConfig>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + AssetFeeTier::INIT_SPACE,
        seeds = [SEED_ASSET_FEE_TIER, app_id.as_ref(), mint.as_ref()],
        bump,
    )]
    pub asset_fee_tier: Account<'info, AssetFeeTier>,
    pub system_program: Program<'info, System>,
}

/// `mint` is a key, not a validated account — EVM keys tiers by raw asset
/// address; registration is admin-gated either way.
pub(crate) fn set_asset_fee_tier_handler(
    ctx: Context<SetAssetFeeTier>,
    app_id: [u8; 32],
    mint: Pubkey,
    threshold: u64,
    below_bps: u16,
    above_or_equal_bps: u16,
) -> Result<()> {
    require!(
        below_bps <= MAX_FEE_BPS && above_or_equal_bps <= MAX_FEE_BPS,
        SraError::FeeTooHigh
    );

    let asset_fee_tier = &mut ctx.accounts.asset_fee_tier;
    asset_fee_tier.app_id = app_id;
    asset_fee_tier.mint = mint;
    asset_fee_tier.threshold = threshold;
    asset_fee_tier.below_bps = below_bps;
    asset_fee_tier.above_or_equal_bps = above_or_equal_bps;
    asset_fee_tier.bump = ctx.bumps.asset_fee_tier;

    emit!(AssetFeeTierUpdated {
        app_id: asset_fee_tier.app_id,
        mint: asset_fee_tier.mint,
        threshold: asset_fee_tier.threshold,
        below_bps: asset_fee_tier.below_bps,
        above_or_equal_bps: asset_fee_tier.above_or_equal_bps,
    });

    Ok(())
}

/// Deliberately does NOT require a live `ProtocolFeeConfig` (divergence from
/// EVM, which gates tier writes on `pf.isSet`): this permits cleaning up — and
/// reclaiming rent from — tiers left dangling by `remove_protocol_fee`, which
/// EVM strands until re-registration.
#[derive(Accounts)]
#[instruction(app_id: [u8; 32], mint: Pubkey)]
pub struct RemoveAssetFeeTier<'info> {
    /// Receives the closed PDA's rent.
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        mut,
        close = admin,
        seeds = [SEED_ASSET_FEE_TIER, app_id.as_ref(), mint.as_ref()],
        bump = asset_fee_tier.bump,
    )]
    pub asset_fee_tier: Account<'info, AssetFeeTier>,
}

pub(crate) fn remove_asset_fee_tier_handler(
    ctx: Context<RemoveAssetFeeTier>,
    _app_id: [u8; 32],
    _mint: Pubkey,
) -> Result<()> {
    let asset_fee_tier = &ctx.accounts.asset_fee_tier;
    emit!(AssetFeeTierRemoved {
        app_id: asset_fee_tier.app_id,
        mint: asset_fee_tier.mint,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// 2-step admin transfer
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct SetPendingAdmin<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
}

pub(crate) fn set_pending_admin_handler(
    ctx: Context<SetPendingAdmin>,
    new_pending_admin: Pubkey,
) -> Result<()> {
    // EVM parity: setPendingAdmin(address(0)) reverts.
    require!(
        new_pending_admin != Pubkey::default(),
        SraError::InvalidPendingAdmin
    );

    let fee_config = &mut ctx.accounts.fee_config;
    fee_config.pending_admin = new_pending_admin;

    emit!(PendingAdminSet {
        admin: fee_config.admin,
        pending_admin: fee_config.pending_admin,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    pub pending_admin: Signer<'info>,
    #[account(mut, seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = pending_admin)]
    pub fee_config: Account<'info, FeeConfig>,
}

pub(crate) fn accept_admin_handler(ctx: Context<AcceptAdmin>) -> Result<()> {
    let fee_config = &mut ctx.accounts.fee_config;
    let old_admin = fee_config.admin;
    fee_config.admin = ctx.accounts.pending_admin.key();
    fee_config.pending_admin = Pubkey::default();

    emit!(AdminTransferred {
        old_admin,
        new_admin: fee_config.admin,
    });

    Ok(())
}
