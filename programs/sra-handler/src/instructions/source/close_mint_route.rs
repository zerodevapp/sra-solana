use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;
use sra_common::{
    errors::SraError,
    types::{SraConfig, SraMintRoute},
};

/// Closes an `SraMintRoute` and returns rent to the owner.
///
/// The associated `MintNonce` is intentionally **not** closed — nonces persist
/// across a `close_mint_route` + `add_mint_route` cycle so that a recreated
/// route cannot reuse a prepared-message nonce, which the EVM side replays into
/// `SRAHandler._replayKey` to drop duplicate deposits.
///
/// `sra_vault.amount == 0` is enforced as an account constraint so the check
/// runs before `close = owner` fires; draining must happen first via
/// `update_mint_route(paused=true)` + an existing flow that empties the vault.
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
        constraint = sra_vault.amount == 0 @ SraError::VaultNotEmpty,
    )]
    pub mint_route: Account<'info, SraMintRoute>,
    #[account(
        associated_token::mint = mint_route.source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Account<'info, TokenAccount>,
}

pub(crate) fn handler(_ctx: Context<CloseMintRoute>) -> Result<()> {
    // All validation + the actual close are declarative on the Accounts
    // struct (`close = owner`, `constraint = sra_vault.amount == 0`). Nothing
    // to do imperatively: the MintNonce is deliberately absent from the
    // struct so it survives.
    Ok(())
}
