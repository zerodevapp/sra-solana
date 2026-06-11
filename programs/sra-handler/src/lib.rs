use anchor_lang::prelude::*;
use sra_common::{source_deposit::PrepareParams, types::SourceHandlerData};

pub mod events;
pub mod instructions;
pub mod utils;

use instructions::*;

declare_id!("Gte7LViTnE2UixBF59Xfu2sSqcmHhMNANj42yUkYpkdo");

#[program]
pub mod sra_handler {
    use super::*;

    pub fn initialize_sra(
        ctx: Context<InitializeSra>,
        config_hash: [u8; 32],
        handler_data: SourceHandlerData,
        salt: [u8; 32],
    ) -> Result<()> {
        instructions::source::initialize_sra::handler(ctx, config_hash, handler_data, salt)
    }

    pub fn initialize_address_book_root(ctx: Context<InitializeAddressBookRoot>) -> Result<()> {
        instructions::source::address_book::initialize_root_handler(ctx)
    }

    pub fn set_address_book_entry(
        ctx: Context<SetAddressBookEntry>,
        source_mint: Pubkey,
        target_chain_id: u64,
        target_token: [u8; 32],
        target_decimals: u8,
    ) -> Result<()> {
        instructions::source::address_book::set_entry_handler(
            ctx,
            source_mint,
            target_chain_id,
            target_token,
            target_decimals,
        )
    }

    pub fn validate_and_prepare(
        ctx: Context<ValidateAndPrepare>,
        handler_data: SourceHandlerData,
        params: PrepareParams,
        paired_adapter_ix_data_hash: [u8; 32],
    ) -> Result<()> {
        instructions::source::validate_and_prepare::handler(
            ctx,
            handler_data,
            params,
            paired_adapter_ix_data_hash,
        )
    }

    pub fn initialize_fee_config(
        ctx: Context<InitializeFeeConfig>,
        protocol_fee_bps: u16,
        protocol_fee_recipient: Pubkey,
    ) -> Result<()> {
        instructions::shared::fees::initialize_fee_config_handler(
            ctx,
            protocol_fee_bps,
            protocol_fee_recipient,
        )
    }

    pub fn register_app_fee(
        ctx: Context<RegisterAppFee>,
        app_id: [u8; 32],
        app_fee_bps: u16,
        app_fee_recipient: Pubkey,
    ) -> Result<()> {
        instructions::shared::fees::register_app_fee_handler(
            ctx,
            app_id,
            app_fee_bps,
            app_fee_recipient,
        )
    }
}
