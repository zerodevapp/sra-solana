use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;
pub mod utils;

pub use errors::*;
pub use instructions::*;

declare_id!("HBfktmY2oiAmP1m6baQAy9ACJ9uKuyTFyJtHdVYRBL4");

#[program]
pub mod sra_cctp_adapter {
    use super::*;

    pub fn execute_deposit_cctp(
        ctx: Context<ExecuteDepositCctp>,
        params: sra_common::source_deposit::CctpDepositParams,
    ) -> Result<()> {
        instructions::execute_deposit::handler(ctx, params)
    }

    pub fn recover_stuck_custody(ctx: Context<RecoverStuckCustody>) -> Result<()> {
        instructions::recover_custody::handler(ctx)
    }

    pub fn verify_cctp_mint(
        ctx: Context<VerifyCctpMint>,
        nonce: [u8; 32],
        message: Vec<u8>,
        attestation: Vec<u8>,
    ) -> Result<()> {
        instructions::verify_mint::handler(ctx, nonce, message, attestation)
    }
}
