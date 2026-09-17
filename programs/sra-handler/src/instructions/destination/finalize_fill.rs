//! Destination fill finalization.
//!
//! Consumes the adapter-written `VerifiedFill`, moves the full verified amount
//! from the route vault into the execution vault, and writes the `FillReceipt`
//! replay marker. No fee is deducted — fees are collected on the source deposit.
//!
//! Running the route's actions is a separate instruction, so finalization
//! always leaves the receipt `Finalized` (funds staged, no actions attempted).
//!
//! Every routing/amount value comes only from the relay-hash-bound
//! `VerifiedFill`; `fill_id` only seeds the receipt PDA and is cross-checked
//! against the evidence.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::{
    errors::SraError,
    seeds::{SEED_EXECUTION, SEED_FILL_RECEIPT, SEED_SRA_ROUTE, SEED_VERIFIED_FILL},
    types::{ExecutionStatus, FillReceipt, SolanaSraRoute, VerifiedFillData},
};

use crate::events::FillFinalized;

#[derive(Accounts)]
#[instruction(fill_id: [u8; 32], route_config_hash: [u8; 32])]
pub struct FinalizeFill<'info> {
    /// Permissionless caller (relayer / cranker). Pays the receipt rent.
    #[account(mut)]
    pub payer: Signer<'info>,
    // `route_config_hash` is not stored on the route, so it is passed as an
    // argument; binding it in the seeds both validates it and yields the signer
    // seeds for the vault transfer below.
    #[account(
        seeds = [SEED_SRA_ROUTE, route.owner.as_ref(), &route_config_hash, &route.salt],
        bump = route.bump,
    )]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// CHECK: adapter-owned `VerifiedFill`, read as raw bytes. The body checks the
    /// owner ∈ `route.allowed_adapters`, pins the account to its canonical PDA, and
    /// validates the layout via `VerifiedFillData::deserialize_from_account`.
    pub verified_fill: UncheckedAccount<'info>,
    /// The replay marker. `init` (never `init_if_needed`) — a second
    /// finalization of the same fill collides on this PDA and aborts.
    #[account(
        init,
        payer = payer,
        space = 8 + FillReceipt::INIT_SPACE,
        seeds = [SEED_FILL_RECEIPT, route.key().as_ref(), &fill_id],
        bump,
    )]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    /// Bound to the route's committed mint; decimals are read live for
    /// `transfer_checked` (never snapshotted).
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = route,
    )]
    pub route_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA ["execution", route] — the execution-vault authority.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(
    ctx: Context<FinalizeFill>,
    fill_id: [u8; 32],
    route_config_hash: [u8; 32],
) -> Result<()> {
    // 1. Route must be active (pause blocks finalize, so a paused fill stays finalizable after unpause).
    require!(!ctx.accounts.route.paused, SraError::RoutePaused);

    // 2. Evidence must come from an allowlisted adapter (its program-owner ∈ the allowlist).
    let verified_fill_owner = *ctx.accounts.verified_fill.owner;
    require!(
        ctx.accounts
            .route
            .allowed_adapters
            .contains(&verified_fill_owner),
        SraError::AdapterNotAllowed
    );

    // 2b. Pin the evidence to its canonical PDA. The owner check alone is not
    //     enough: an account merely *owned* by an allowlisted adapter can be
    //     fabricated (create-elsewhere-then-reassign), but only the adapter's own
    //     `verify_across_fill` init can place data at this PDA, and that requires
    //     a genuine Across fill. The address is bound to the same `fill_id` arg
    //     that seeds the receipt, so the receipt and the evidence name one fill.
    let (expected_verified_fill, _) = Pubkey::find_program_address(
        &[SEED_VERIFIED_FILL, verified_fill_owner.as_ref(), &fill_id],
        &verified_fill_owner,
    );
    require_keys_eq!(
        ctx.accounts.verified_fill.key(),
        expected_verified_fill,
        SraError::InvalidFill
    );

    // 3. Decode the `VerifiedFill` (discriminator + exact-layout checks; type-confusion defense).
    let verified = {
        let data = ctx.accounts.verified_fill.try_borrow_data()?;
        VerifiedFillData::deserialize_from_account(&data)?
    };

    // 4-8. Re-check every committed field against the route (defense-in-depth across the adapter boundary).
    let route_key = ctx.accounts.route.key();
    // The self-declared adapter must match the account's real program-owner (the
    // PDA pin above already binds it; this also rejects stale/cross-adapter bytes).
    require_keys_eq!(verified.adapter, verified_fill_owner, SraError::InvalidFill);
    require!(fill_id == verified.fill_id, SraError::InvalidFill);
    require!(verified.route == route_key, SraError::InvalidFill);
    require!(
        verified.mint == ctx.accounts.route.expected_mint,
        SraError::MintMismatch
    );
    require!(
        verified.source_chain_id == ctx.accounts.route.source_chain_id,
        SraError::ChainMismatch
    );
    require!(
        verified.amount >= verified.expected_amount,
        SraError::FillAmountTooLow
    );

    // 9. Vault must cover the amount — Across fills are exact, so a short vault is rejected (stays recoverable).
    require!(
        ctx.accounts.route_vault.amount >= verified.amount,
        SraError::InsufficientVaultBalance
    );

    // 10. Move the full amount route_vault -> execution_vault, no fee (collected on the source); the route PDA signs.
    let owner_bytes = ctx.accounts.route.owner.to_bytes();
    let salt = ctx.accounts.route.salt;
    let bump_arr = [ctx.accounts.route.bump];
    let seeds: &[&[u8]] = &[
        SEED_SRA_ROUTE,
        owner_bytes.as_ref(),
        route_config_hash.as_ref(),
        salt.as_ref(),
        bump_arr.as_ref(),
    ];
    let signer_seeds: &[&[&[u8]]] = &[seeds];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.route_vault.to_account_info(),
                mint: ctx.accounts.expected_mint.to_account_info(),
                to: ctx.accounts.execution_vault.to_account_info(),
                authority: ctx.accounts.route.to_account_info(),
            },
            signer_seeds,
        ),
        verified.amount,
        ctx.accounts.expected_mint.decimals,
    )?;

    // Write the replay marker + lifecycle state (`init` guaranteed this is the first finalization).
    let finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.fill_receipt.set_inner(FillReceipt {
        route: route_key,
        fill_id,
        amount: verified.amount,
        expected_amount: verified.expected_amount,
        execution_status: ExecutionStatus::Finalized,
        finalized_at,
        bump: ctx.bumps.fill_receipt,
    });

    emit!(FillFinalized {
        route: route_key,
        fill_id,
        amount: verified.amount,
        expected_amount: verified.expected_amount,
        execution_status: ExecutionStatus::Finalized,
        finalized_at,
    });

    Ok(())
}
