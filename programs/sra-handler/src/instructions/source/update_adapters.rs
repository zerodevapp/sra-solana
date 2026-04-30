use anchor_lang::prelude::*;
use sra_common::{constants::MAX_ADAPTERS, errors::SraError, types::SraConfig};

use crate::events::AdaptersUpdated;

#[derive(Accounts)]
pub struct UpdateAdapters<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<UpdateAdapters>, adapters: Vec<Pubkey>) -> Result<()> {
    require!(adapters.len() <= MAX_ADAPTERS, SraError::TooManyAdapters);
    ctx.accounts.sra_config.allowed_adapters = adapters;

    emit!(AdaptersUpdated {
        sra_config: ctx.accounts.sra_config.key(),
        adapters: ctx.accounts.sra_config.allowed_adapters.clone(),
    });

    Ok(())
}
