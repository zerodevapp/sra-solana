//! Owner-attested Across fill verification after the canonical status PDA is closed.
//!
//! Across permits the recorded relayer to close a `FillStatusAccount` once its
//! fill deadline has passed, which permanently breaks `verify_across_fill` for a
//! fill that was delivered but never verified. This fallback substitutes the
//! affected route owner's signature for the destroyed on-chain proof: it accepts
//! only the canonical, now System-owned, data-empty status PDA after the
//! deadline, revalidates the relay data against the route exactly like the live
//! path, and creates the ordinary `VerifiedFill`, so `finalize_fill` and all
//! replay protection remain unchanged.

use anchor_lang::prelude::*;
use sra_common::{constants::ACROSS_SPOKE_POOL_ID, seeds::SEED_VERIFIED_FILL, types::SolanaSraRoute};

use crate::events::AcrossFillVerifiedAfterClose;
use crate::instructions::verify_fill::{validated_verified_fill, FILL_STATUS_SEED};
use crate::state::VerifiedFill;
use crate::utils::svm_spoke::types::RelayData;
use crate::AcrossAdapterError;

#[derive(Accounts)]
#[instruction(relay_hash: [u8; 32])]
pub struct VerifyAcrossFillAfterClose<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    /// CHECK: Canonical Across fill-status PDA. The handler requires the closed
    /// representation: System-owned and data-empty. Lamports are ignored so a
    /// dust transfer cannot disable recovery.
    #[account(
        seeds = [FILL_STATUS_SEED, relay_hash.as_ref()],
        bump,
        seeds::program = across_program.key(),
    )]
    pub fill_status: UncheckedAccount<'info>,
    #[account(has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    #[account(
        init,
        payer = payer,
        space = 8 + VerifiedFill::INIT_SPACE,
        seeds = [SEED_VERIFIED_FILL, crate::ID.as_ref(), relay_hash.as_ref()],
        bump,
    )]
    pub verified_fill: Box<Account<'info, VerifiedFill>>,
    /// CHECK: canonical Across SpokePool program, pinned by address.
    #[account(address = ACROSS_SPOKE_POOL_ID)]
    pub across_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<VerifyAcrossFillAfterClose>,
    relay_hash: [u8; 32],
    relay_data: RelayData,
) -> Result<()> {
    require_keys_eq!(
        *ctx.accounts.fill_status.owner,
        anchor_lang::system_program::ID,
        AcrossAdapterError::FillStatusNotClosed
    );
    require!(
        ctx.accounts.fill_status.data_is_empty(),
        AcrossAdapterError::FillStatusNotClosed
    );
    require!(
        Clock::get()?.unix_timestamp > i64::from(relay_data.fill_deadline),
        AcrossAdapterError::FillDeadlineNotPassed
    );

    let verified = validated_verified_fill(
        &ctx.accounts.route,
        relay_hash,
        &relay_data,
        ctx.bumps.verified_fill,
    )?;
    ctx.accounts.verified_fill.set_inner(verified);

    emit!(AcrossFillVerifiedAfterClose {
        verified_fill: ctx.accounts.verified_fill.key(),
        route: ctx.accounts.route.key(),
        owner: ctx.accounts.owner.key(),
        mint: relay_data.output_token,
        fill_id: relay_hash,
        amount: relay_data.output_amount,
        expected_amount: relay_data.output_amount,
        source_chain_id: relay_data.origin_chain_id,
    });

    Ok(())
}
