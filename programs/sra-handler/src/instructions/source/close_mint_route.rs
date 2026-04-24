use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;
use sra_common::{
    errors::SraError,
    seeds::SEED_ADAPTER_AUTHORITY,
    types::{SraConfig, SraMintRoute},
};

/// Closes an `SraMintRoute` and returns rent to the owner.
///
/// The associated `MintNonce` is intentionally **not** closed — nonces persist
/// across a `close_mint_route` + `add_mint_route` cycle so that a recreated
/// route cannot reuse a prepared-message nonce, which the EVM side replays into
/// `SRAHandler._replayKey` to drop duplicate deposits.
///
/// The route must already be paused and both route-owned custody accounts must
/// be empty before close. That keeps close/recreate safe once deposits have
/// started using the route.
#[derive(Accounts)]
pub struct CloseMintRoute<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
    #[account(
        mut,
        close = owner,
        has_one = sra_config,
        constraint = mint_route.paused @ SraError::RouteMustBePaused,
        constraint = sra_vault.amount == 0 @ SraError::VaultNotEmpty,
    )]
    pub mint_route: Account<'info, SraMintRoute>,
    #[account(
        associated_token::mint = mint_route.source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Account<'info, TokenAccount>,
    /// CHECK: pinned to the adapter stored on the route being closed.
    #[account(address = mint_route.adapter_program)]
    pub adapter_program: UncheckedAccount<'info>,
    /// CHECK: validated via seeds under the stored `adapter_program`.
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
        seeds::program = adapter_program.key(),
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    #[account(
        associated_token::mint = mint_route.source_mint,
        associated_token::authority = adapter_authority,
        constraint = adapter_custody.amount == 0 @ SraError::AdapterCustodyNotEmpty,
    )]
    pub adapter_custody: Account<'info, TokenAccount>,
}

pub(crate) fn handler(_ctx: Context<CloseMintRoute>) -> Result<()> {
    // All validation + the actual close are declarative on the Accounts
    // struct. Nothing to do imperatively: the MintNonce is deliberately absent
    // from the struct so it survives.
    Ok(())
}
