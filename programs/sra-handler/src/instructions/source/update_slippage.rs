use anchor_lang::prelude::*;
use sra_common::{constants::MAX_SLIPPAGE_BPS, errors::SraError, types::SraConfig};

#[derive(Accounts)]
pub struct UpdateSlippage<'info> {
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
}

pub(crate) fn handler(ctx: Context<UpdateSlippage>, allowed_slippage_bps: u16) -> Result<()> {
    require!(
        allowed_slippage_bps <= MAX_SLIPPAGE_BPS,
        SraError::InvalidSlippage
    );

    ctx.accounts.sra_config.allowed_slippage_bps = allowed_slippage_bps;
    Ok(())
}
