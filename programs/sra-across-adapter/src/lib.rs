use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;
pub mod utils;

pub use errors::*;
pub use instructions::*;

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

    pub fn recover_stuck_custody(ctx: Context<RecoverStuckCustody>) -> Result<()> {
        instructions::recover_custody::handler(ctx)
    }

    pub fn verify_across_fill(
        ctx: Context<VerifyAcrossFill>,
        relay_hash: [u8; 32],
        relay_data: crate::utils::svm_spoke::types::RelayData,
    ) -> Result<()> {
        instructions::verify_fill::handler(ctx, relay_hash, relay_data)
    }
}
