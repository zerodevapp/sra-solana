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
    // The committed mint binds the route identity (it is inside route_config_hash)
    // AND both vault ATAs to exactly one asset, so one route authorizes exactly
    // one mint. Decimals are read live at fill time, so there is no snapshot to drift.
    #[account(address = params.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = expected_mint,
        associated_token::authority = route,
    )]
    pub route_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA ["execution", route] — the execution-vault authority. Holds no
    /// data; validated by seeds and signs later via invoke_signed.
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
    // Fail-fast bounds first — cheapest checks, before any hashing or adapter I/O.
    require!(
        params.action_config.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );
    require!(
        params.allowed_adapters.len() <= MAX_ADAPTERS,
        SraError::TooManyAdapters
    );

    // Σ SplTransfer.amount_bps may not exceed 100% of the vault. VerifiedCpi
    // entries are bounded by their own input_bps and validated per-variant.
    let mut spl_transfer_bps: u32 = 0;
    for action in &params.action_config {
        match action {
            SolanaAction::SplTransfer { amount_bps, .. } => {
                // checked_add is belt-and-suspenders: with the length bound above
                // the sum is at most 4 * u16::MAX, which cannot overflow a u32.
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

    // Every allowlisted adapter must be a deployed, loader-owned, executable
    // program *at creation time* — a mistyped pubkey must not become a trusted
    // fill attester. The adapter accounts ride in remaining_accounts, one per
    // entry, in the same order.
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

    // Bind the supplied seed hash to the params: explicit-arg + in-body recheck,
    // because Anchor cannot express a method-call seed. route_config_hash stays a
    // pure helper, so an invalid action list fails here, never inside try_accounts.
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
