use anchor_lang::prelude::*;
use sra_common::{errors::SraError, types::SraConfig};

use crate::events::SraManaged;

#[derive(Accounts)]
pub struct ManageSra<'info> {
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner)]
    pub sra_config: Box<Account<'info, SraConfig>>,
}

pub(crate) fn handler(ctx: Context<ManageSra>, initiator: Pubkey, active: bool) -> Result<()> {
    require_keys_neq!(initiator, Pubkey::default(), SraError::InvalidInitiator);
    let sra_config = &mut ctx.accounts.sra_config;
    sra_config.initiator = initiator;
    sra_config.active = active;

    emit!(SraManaged {
        sra_config: sra_config.key(),
        owner: sra_config.owner,
        initiator: sra_config.initiator,
        active: sra_config.active,
        deposit_nonce: sra_config.deposit_nonce,
    });

    Ok(())
}
