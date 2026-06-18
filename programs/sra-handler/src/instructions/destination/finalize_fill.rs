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
    seeds::{SEED_EXECUTION, SEED_FILL_RECEIPT, SEED_SRA_ROUTE},
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
    /// CHECK: adapter-owned `VerifiedFill`. Read as raw bytes — the owner is
    /// checked against `route.allowed_adapters` and the layout is validated by
    /// `VerifiedFillData::deserialize_from_account` in the body (discriminator +
    /// exact-length), so no Anchor type/owner check applies here.
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
    // 1. The route must be active. Pause gates fund movement (not the upstream
    //    evidence-recording in the adapter), so a fill recorded while paused
    //    becomes finalizable again once the owner unpauses.
    require!(!ctx.accounts.route.paused, SraError::RoutePaused);

    // 2. The evidence must be written by a currently-allowlisted adapter program
    //    (its program-owner ∈ the route allowlist).
    let verified_fill_owner = *ctx.accounts.verified_fill.owner;
    require!(
        ctx.accounts
            .route
            .allowed_adapters
            .contains(&verified_fill_owner),
        SraError::AdapterNotAllowed
    );

    // 3. Deserialize the `VerifiedFill` with discriminator + exact-layout checks
    //    (type-confusion / schema-drift defense across the program boundary).
    let verified = {
        let data = ctx.accounts.verified_fill.try_borrow_data()?;
        VerifiedFillData::deserialize_from_account(&data)?
    };

    // 4-8. Cross-check every committed field against the route. The adapter
    //       already checked the mint, but re-checking here defends the
    //       adapter -> handler trust boundary: evidence valid across multiple
    //       mints/routes would be a privilege-expansion bug.
    let route_key = ctx.accounts.route.key();
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

    // 9. The route vault must hold at least the verified amount. Across fills
    //    deliver an exact amount, so a short vault is an anomaly: reject and
    //    leave the funds recoverable rather than forward a partial amount.
    require!(
        ctx.accounts.route_vault.amount >= verified.amount,
        SraError::InsufficientVaultBalance
    );

    // 10. Move the full verified amount route_vault -> execution_vault with no
    //     fee deducted (fees were collected on the source). The route PDA owns
    //     the route vault and signs the transfer.
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
            ctx.accounts.token_program.to_account_info(),
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

    // Write the replay marker + lifecycle state. `init` already guaranteed this
    // is the first finalization of `fill_id` for this route.
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
