//! Destination fill verification.
//!
//! Records a `VerifiedFill` once this adapter has independently confirmed an
//! Across fill. Unlike the source side there is no CPI into the SpokePool to
//! backstop validation — every check is the adapter's own:
//!
//! 1. The `fill_status` account is pinned to `["fills", relay_hash]` under the
//!    Across program and owner-checked, and its status must be `Filled`.
//! 2. Re-deriving the relay hash over the caller-supplied `relay_data` must
//!    reproduce that same `relay_hash` — the cryptographic binding of the data
//!    to the Across-verified fill. Any mutated field yields a different hash.
//! 3. The verified routing fields are cross-checked against the committed route.
//!
//! The resulting `VerifiedFill` is the trust boundary the handler reads (across
//! the program boundary) to authorize fund movement; a vault-balance increase
//! alone never proves a legitimate fill.

use anchor_lang::prelude::*;
use sra_common::{
    constants::{ACROSS_SPOKE_POOL_ID, SOLANA_CHAIN_ID},
    seeds::SEED_VERIFIED_FILL,
    types::SolanaSraRoute,
};

use crate::events::AcrossFillVerified;
use crate::state::VerifiedFill;
use crate::utils::fill_status::deserialize_fill_status;
use crate::utils::relay_hash::compute_relay_hash;
use crate::utils::svm_spoke::types::{FillStatus, RelayData};
use crate::AcrossAdapterError;

/// Fill-status PDA seed prefix the deployed Across SpokePool uses
/// (`["fills", relay_hash]`).
const FILL_STATUS_SEED: &[u8] = b"fills";

#[derive(Accounts)]
#[instruction(relay_hash: [u8; 32])]
pub struct VerifyAcrossFill<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Across `FillStatusAccount` at `["fills", relay_hash]` under the
    /// SpokePool. Pinned by seeds + owner; read as raw bytes (never CPI'd), so
    /// its layout is validated by `deserialize_fill_status` in the handler body.
    #[account(
        seeds = [FILL_STATUS_SEED, relay_hash.as_ref()],
        bump,
        seeds::program = across_program.key(),
        owner = across_program.key(),
    )]
    pub fill_status: UncheckedAccount<'info>,
    /// The committed destination route the fill is being verified against. An
    /// `Account` here requires it to be a genuine handler-owned route.
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
    ctx: Context<VerifyAcrossFill>,
    relay_hash: [u8; 32],
    relay_data: RelayData,
) -> Result<()> {
    // 1. The fill must be completed. The account is already pinned to
    //    `["fills", relay_hash]` under the Across program and owner-checked; read
    //    its status with the exact-layout deserializer.
    let status = {
        let data = ctx.accounts.fill_status.try_borrow_data()?;
        deserialize_fill_status(&data)?
    };
    require!(
        matches!(status, FillStatus::Filled),
        AcrossAdapterError::FillNotCompleted
    );

    // 2. Bind the caller-supplied relay data to the verified fill: re-deriving the
    //    relay hash over `relay_data` must reproduce the hash that seeds the
    //    fill-status PDA. The fill lands on this chain, so the bound chain id is
    //    this chain's id.
    require!(
        compute_relay_hash(&relay_data, SOLANA_CHAIN_ID)? == relay_hash,
        AcrossAdapterError::RelayHashMismatch
    );

    // 3. Cross-check the (now relay-hash-bound) routing fields against the
    //    committed route.
    let route_key = ctx.accounts.route.key();
    require_keys_eq!(
        relay_data.recipient,
        route_key,
        AcrossAdapterError::RecipientMismatch
    );
    require_keys_eq!(
        relay_data.output_token,
        ctx.accounts.route.expected_mint,
        AcrossAdapterError::OutputTokenMismatch
    );
    require_eq!(
        relay_data.origin_chain_id,
        ctx.accounts.route.source_chain_id,
        AcrossAdapterError::OriginChainMismatch
    );

    // 4. Record the verification artifact from the bound relay data. Across binds a
    //    single exact-fill output amount, so it is both the delivered amount and
    //    the source-committed minimum the handler later enforces. `output_token`
    //    has been checked equal to the route mint; persisting it names the asset
    //    this fill delivered.
    ctx.accounts.verified_fill.set_inner(VerifiedFill {
        adapter: crate::ID,
        route: route_key,
        fill_id: relay_hash,
        mint: relay_data.output_token,
        amount: relay_data.output_amount,
        expected_amount: relay_data.output_amount,
        source_chain_id: relay_data.origin_chain_id,
        created_at: Clock::get()?.unix_timestamp,
        bump: ctx.bumps.verified_fill,
    });

    emit!(AcrossFillVerified {
        verified_fill: ctx.accounts.verified_fill.key(),
        route: route_key,
        mint: relay_data.output_token,
        fill_id: relay_hash,
        amount: relay_data.output_amount,
        expected_amount: relay_data.output_amount,
        source_chain_id: relay_data.origin_chain_id,
    });

    Ok(())
}
