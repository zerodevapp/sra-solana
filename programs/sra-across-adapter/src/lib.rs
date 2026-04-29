use anchor_lang::prelude::*;

pub mod errors;
pub mod instructions;
pub mod utils;

pub use errors::*;
pub use instructions::*;
pub use utils::*;

declare_id!("8n167Xs4Xx7zEq5fuyCDVgYd3vUjFmHAkGTdK2SCpmXD");

#[program]
pub mod sra_across_adapter {
    use super::*;

    pub fn execute_deposit_across(
        ctx: Context<ExecuteDepositAcross>,
        params: sra_common::source_deposit::AcrossDepositParams,
    ) -> Result<()> {
        instructions::execute_deposit::handler(ctx, params)
    }
}
