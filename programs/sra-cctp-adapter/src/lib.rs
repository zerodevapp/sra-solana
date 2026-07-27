use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;

pub use errors::*;
pub use instructions::*;

declare_id!("2bXAvxF3U4FtWAnJtcL9LDnwM7qnErf3y3m6USLRKpRw");

#[program]
pub mod sra_cctp_adapter {
    use super::*;

    pub fn recover_stuck_custody(ctx: Context<RecoverStuckCustody>) -> Result<()> {
        instructions::recover_custody::handler(ctx)
    }
}
