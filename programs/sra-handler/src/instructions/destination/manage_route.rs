use anchor_lang::prelude::*;
use sra_common::{
    constants::MAX_ADAPTERS,
    errors::SraError,
    types::{ManageRouteParams, SolanaSraRoute},
};

use crate::events::RouteManaged;
use crate::utils::adapter::{require_distinct_adapters, validate_adapter_accounts};
use crate::utils::executor::validate_executors;

#[derive(Accounts)]
pub struct ManageRoute<'info> {
    pub owner: Signer<'info>,
    // allowed_adapters reserves max_len(MAX_ADAPTERS) at create — edits never realloc.
    #[account(mut, has_one = owner)]
    pub route: Box<Account<'info, SolanaSraRoute>>,
}

pub(crate) fn handler(ctx: Context<ManageRoute>, params: ManageRouteParams) -> Result<()> {
    let route = &mut ctx.accounts.route;

    if let Some(paused) = params.pause {
        route.paused = paused;
    }

    // Whole-list replace (EVM _installAdapters): validate the new set, then swap.
    if let Some(adapters) = params.adapters {
        require!(adapters.len() <= MAX_ADAPTERS, SraError::TooManyAdapters);
        require_distinct_adapters(&adapters)?;
        validate_adapter_accounts(&adapters, ctx.remaining_accounts)?;
        route.allowed_adapters = adapters;
    }

    // Whole-list replace, mirroring adapters: validate shape (bounded + non-zero
    // + distinct), then swap. `Some(vec![])` clears (freezes execute_actions).
    if let Some(executors) = params.executors {
        validate_executors(&executors)?;
        route.allowed_executors = executors;
    }

    emit!(RouteManaged {
        route: route.key(),
        owner: route.owner,
        allowed_adapters: route.allowed_adapters.clone(),
        allowed_executors: route.allowed_executors.clone(),
        paused: route.paused,
    });

    Ok(())
}
