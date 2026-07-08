use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
use sra_common::{
    constants::MAX_ADAPTERS,
    errors::SraError,
    seeds::{route_config_hash as compute_route_config_hash, SEED_EXECUTION, SEED_SRA_ROUTE},
    types::{validate_action_config, CreateRouteParams, SolanaSraRoute},
};

use crate::events::RouteCreated;
use crate::utils::adapter::{require_distinct_adapters, validate_adapter_accounts};
use crate::utils::executor::validate_executors;

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
    // Fail-fast validation before any hashing or adapter I/O: action-config
    // bounds + per-variant checks (combined bps == 100%, non-zero keys,
    // template pinning, oracle refused), then the adapter-list bound.
    validate_action_config(&params.action_config)?;
    require!(
        params.allowed_adapters.len() <= MAX_ADAPTERS,
        SraError::TooManyAdapters
    );
    require_distinct_adapters(&params.allowed_adapters)?;
    // Shape-only (bounded/non-zero/distinct); no loader check, empty allowed.
    validate_executors(&params.allowed_executors)?;

    validate_adapter_accounts(&params.allowed_adapters, ctx.remaining_accounts)?;

    // Explicit seed arg + in-body recheck (Anchor can't express a method-call
    // seed); the pure helper means a bad action list fails here, not in try_accounts.
    let expected_route_config_hash = compute_route_config_hash(
        &ctx.accounts.owner.key(),
        params.source_chain_id,
        &params.expected_mint,
        &params.action_config,
    )?;
    require!(
        expected_route_config_hash == route_config_hash,
        SraError::InvalidRouteConfig
    );

    let route = &mut ctx.accounts.route;
    route.owner = ctx.accounts.owner.key();
    route.source_chain_id = params.source_chain_id;
    route.expected_mint = params.expected_mint;
    route.action_config = params.action_config.clone();
    route.salt = params.salt;
    route.allowed_adapters = params.allowed_adapters;
    // Not in route_config_hash (mutable, like allowed_adapters).
    route.allowed_executors = params.allowed_executors;
    route.paused = false;
    route.bump = ctx.bumps.route;

    emit!(RouteCreated {
        route: route.key(),
        owner: route.owner,
        source_chain_id: route.source_chain_id,
        expected_mint: route.expected_mint,
        salt: route.salt,
        allowed_adapters: route.allowed_adapters.clone(),
        allowed_executors: route.allowed_executors.clone(),
        paused: route.paused,
    });

    Ok(())
}
