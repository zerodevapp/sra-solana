use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
use sra_common::{
    constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS, MAX_ADAPTERS},
    errors::SraError,
    seeds::{
        action_config_hash as compute_action_config_hash,
        route_config_hash as compute_route_config_hash, SEED_EXECUTION, SEED_SRA_ROUTE,
    },
    types::{CreateRouteParams, SolanaAction, SolanaSraRoute},
};

use crate::events::RouteCreated;
use crate::utils::adapter::validate_adapter_program;

#[derive(Accounts)]
#[instruction(route_config_hash: [u8; 32], params: CreateRouteParams)]
pub struct CreateRoute<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + SolanaSraRoute::INIT_SPACE,
        seeds = [SEED_SRA_ROUTE, owner.key().as_ref(), &route_config_hash, &params.salt],
        bump,
    )]
    pub route: Box<Account<'info, SolanaSraRoute>>,
    // Binds the route to one asset (mint is in route_config_hash + both vaults).
    // Decimals are read live at fill time, never snapshotted.
    #[account(address = params.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = expected_mint,
        associated_token::authority = route,
    )]
    pub route_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA ["execution", route] — execution-vault authority; validated by seeds.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(
    ctx: Context<CreateRoute>,
    route_config_hash: [u8; 32],
    params: CreateRouteParams,
) -> Result<()> {
    // Fail-fast bounds before any hashing or adapter I/O.
    require!(
        params.action_config.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );
    require!(
        params.allowed_adapters.len() <= MAX_ADAPTERS,
        SraError::TooManyAdapters
    );

    // Σ SplTransfer.amount_bps must not exceed 100% of the vault.
    let mut spl_transfer_bps: u32 = 0;
    for action in &params.action_config {
        match action {
            SolanaAction::SplTransfer { amount_bps, .. } => {
                // belt-and-suspenders: 4 * u16::MAX can't overflow u32.
                spl_transfer_bps = spl_transfer_bps
                    .checked_add(*amount_bps as u32)
                    .ok_or(SraError::ArithmeticOverflow)?;
            }
        }
    }
    require!(
        spl_transfer_bps <= BPS_DENOMINATOR as u32,
        SraError::ActionBpsSumExceeded
    );

    // Each allowlisted adapter must be loader-owned-executable at create time so a
    // mistyped pubkey can't become a trusted fill attester. Adapter accounts ride
    // in remaining_accounts, one per entry, same order.
    require!(
        ctx.remaining_accounts.len() == params.allowed_adapters.len(),
        SraError::InvalidAdapterProgram
    );
    for (adapter, account) in params
        .allowed_adapters
        .iter()
        .zip(ctx.remaining_accounts.iter())
    {
        require_keys_eq!(account.key(), *adapter, SraError::InvalidAdapterProgram);
        validate_adapter_program(account)?;
    }

    // Explicit seed arg + in-body recheck (Anchor can't express a method-call
    // seed); the pure helper means a bad action list fails here, not in try_accounts.
    let expected_route_config_hash = compute_route_config_hash(
        &ctx.accounts.owner.key(),
        params.source_chain_id,
        &params.expected_mint,
        &params.app_id,
        &params.action_config,
    )?;
    require!(
        expected_route_config_hash == route_config_hash,
        SraError::InvalidRouteConfig
    );

    let action_hash = compute_action_config_hash(&params.action_config)?;

    let route = &mut ctx.accounts.route;
    route.owner = ctx.accounts.owner.key();
    route.source_chain_id = params.source_chain_id;
    route.expected_mint = params.expected_mint;
    route.app_id = params.app_id;
    route.action_config_hash = action_hash;
    route.salt = params.salt;
    route.allowed_adapters = params.allowed_adapters;
    route.paused = false;
    route.bump = ctx.bumps.route;

    emit!(RouteCreated {
        route: route.key(),
        owner: route.owner,
        source_chain_id: route.source_chain_id,
        expected_mint: route.expected_mint,
        app_id: route.app_id,
        action_config_hash: route.action_config_hash,
        salt: route.salt,
        allowed_adapters: route.allowed_adapters.clone(),
        paused: route.paused,
    });

    Ok(())
}
