use anchor_lang::prelude::*;

pub mod events;
pub mod instructions;

use instructions::*;

declare_id!("Gte7LViTnE2UixBF59Xfu2sSqcmHhMNANj42yUkYpkdo");

#[program]
pub mod sra_handler {
    use super::*;

    pub fn initialize_sra(
        ctx: Context<InitializeSra>,
        config: SraInitConfig,
        salt: [u8; 32],
    ) -> Result<()> {
        instructions::source::initialize_sra::handler(ctx, config, salt)
    }

    pub fn update_adapters(ctx: Context<UpdateAdapters>, adapters: Vec<Pubkey>) -> Result<()> {
        instructions::source::update_adapters::handler(ctx, adapters)
    }

    pub fn update_slippage(ctx: Context<UpdateSlippage>, allowed_slippage_bps: u16) -> Result<()> {
        instructions::source::update_slippage::handler(ctx, allowed_slippage_bps)
    }

    pub fn set_address_book_entry(
        ctx: Context<SetAddressBookEntry>,
        target_chain_id: u64,
        target_token: [u8; 32],
        target_decimals: u8,
    ) -> Result<()> {
        instructions::source::address_book::set_entry_handler(
            ctx,
            target_chain_id,
            target_token,
            target_decimals,
        )
    }

    pub fn validate_and_prepare(
        ctx: Context<ValidateAndPrepare>,
        params: PrepareParams,
    ) -> Result<()> {
        instructions::source::validate_and_prepare::handler(ctx, params)
    }
}
