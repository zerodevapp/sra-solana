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
    // allowed_adapters reserves max_len(MAX_ADAPTERS) at create — edits never realloc.
    #[account(mut, has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
}

pub(crate) fn handler(ctx: Context<ManageRoute>, params: ManageRouteParams) -> Result<()> {
    validate_adapter_accounts(&params.add_adapters, ctx.remaining_accounts)?;

    let route = &mut ctx.accounts.route;

    if let Some(paused) = params.pause {
        route.paused = paused;
    }

    route
        .allowed_adapters
        .retain(|adapter| !params.remove_adapters.contains(adapter));
    route
        .allowed_adapters
        .extend_from_slice(&params.add_adapters);

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
