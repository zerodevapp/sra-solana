use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod state;
pub mod utils;

pub use errors::*;
pub use instructions::*;

declare_id!("2bXAvxF3U4FtWAnJtcL9LDnwM7qnErf3y3m6USLRKpRw");

#[program]
pub mod sra_cctp_adapter {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_source_policy(
        ctx: Context<InitializeSourcePolicy>,
        handler_data: sra_common::types::SourceHandlerData,
        salt: [u8; 32],
        executor: Pubkey,
        finality: u32,
        min_gross: u64,
        max_gross: u64,
        max_fee: u64,
    ) -> Result<()> {
        instructions::policy::initialize_source_policy(
            ctx,
            handler_data,
            salt,
            executor,
            finality,
            min_gross,
            max_gross,
            max_fee,
        )
    }

    pub fn set_source_policy(
        ctx: Context<SetSourcePolicy>,
        expected_nonce: u64,
        executor: Pubkey,
        paused: bool,
    ) -> Result<()> {
        instructions::policy::set_source_policy(ctx, expected_nonce, executor, paused)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_destination_policy(
        ctx: Context<InitializeDestinationPolicy>,
        salt: [u8; 32],
        sender: [u8; 32],
        source_token: [u8; 32],
        finality: u32,
        max_fee: u64,
        min_burn: u64,
        max_burn: u64,
    ) -> Result<()> {
        instructions::policy::initialize_destination_policy(
            ctx,
            salt,
            sender,
            source_token,
            finality,
            max_fee,
            min_burn,
            max_burn,
        )
    }

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
