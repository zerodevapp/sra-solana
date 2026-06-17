use anchor_lang::prelude::*;
use sra_common::{
    constants::MAX_ADAPTERS,
    errors::SraError,
    types::{ManageRouteParams, SolanaSraRoute},
};

use crate::events::RouteManaged;
use crate::utils::adapter::{require_distinct_adapters, validate_adapter_accounts};

#[derive(Accounts)]
pub struct ManageRoute<'info> {
    pub owner: Signer<'info>,
    // The adapter allowlist lives within the route's reserved `INIT_SPACE`
    // (max_len = MAX_ADAPTERS), so add/remove never reallocs.
    #[account(mut, has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
}

pub(crate) fn handler(ctx: Context<ManageRoute>, params: ManageRouteParams) -> Result<()> {
    // Validate every newly added adapter up front (read-only), the same gate
    // create_route applies: each must be loader-owned-executable and ride in
    // remaining_accounts, one per entry, in order.
    validate_adapter_accounts(&params.add_adapters, ctx.remaining_accounts)?;

    let route = &mut ctx.accounts.route;

    if let Some(paused) = params.pause {
        route.paused = paused;
    }

    // Remove first so an owner can rotate an adapter (drop the old, add the new)
    // within the MAX_ADAPTERS bound in a single call.
    route
        .allowed_adapters
        .retain(|adapter| !params.remove_adapters.contains(adapter));
    route
        .allowed_adapters
        .extend_from_slice(&params.add_adapters);

    // The allowlist is a set — an add must not duplicate an existing entry or
    // another add — and stays within the slot budget.
    require_distinct_adapters(&route.allowed_adapters)?;
    require!(
        route.allowed_adapters.len() <= MAX_ADAPTERS,
        SraError::TooManyAdapters
    );

    emit!(RouteManaged {
        route: route.key(),
        owner: route.owner,
        allowed_adapters: route.allowed_adapters.clone(),
        paused: route.paused,
    });

    Ok(())
}
