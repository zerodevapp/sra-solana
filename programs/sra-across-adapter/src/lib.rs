use anchor_lang::prelude::*;

pub mod utils;

pub use utils::*;

declare_id!("8n167Xs4Xx7zEq5fuyCDVgYd3vUjFmHAkGTdK2SCpmXD");

#[program]
pub mod sra_across_adapter {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        msg!("Greetings from: {:?}", ctx.program_id);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
