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
        fee_recipient: Pubkey,
    ) -> Result<()> {
        instructions::shared::fees::initialize_fee_config_handler(ctx, fee_recipient)
    }

    pub fn set_fee_recipient(ctx: Context<SetFeeRecipient>, new_recipient: Pubkey) -> Result<()> {
        instructions::shared::fees::set_fee_recipient_handler(ctx, new_recipient)
    }

    pub fn set_protocol_fee(
        ctx: Context<SetProtocolFee>,
        app_id: [u8; 32],
        default_fee_bps: u16,
        allow_override: bool,
    ) -> Result<()> {
        instructions::shared::fees::set_protocol_fee_handler(
            ctx,
            app_id,
            default_fee_bps,
            allow_override,
        )
    }

    pub fn remove_protocol_fee(ctx: Context<RemoveProtocolFee>, app_id: [u8; 32]) -> Result<()> {
        instructions::shared::fees::remove_protocol_fee_handler(ctx, app_id)
    }

    pub fn set_asset_fee_tier(
        ctx: Context<SetAssetFeeTier>,
        app_id: [u8; 32],
        mint: Pubkey,
        threshold: u64,
        below_bps: u16,
        above_or_equal_bps: u16,
    ) -> Result<()> {
        instructions::shared::fees::set_asset_fee_tier_handler(
            ctx,
            app_id,
            mint,
            threshold,
            below_bps,
            above_or_equal_bps,
        )
    }

    pub fn remove_asset_fee_tier(
        ctx: Context<RemoveAssetFeeTier>,
        app_id: [u8; 32],
        mint: Pubkey,
    ) -> Result<()> {
        instructions::shared::fees::remove_asset_fee_tier_handler(ctx, app_id, mint)
    }

    pub fn set_pending_admin(
        ctx: Context<SetPendingAdmin>,
        new_pending_admin: Pubkey,
    ) -> Result<()> {
        instructions::shared::fees::set_pending_admin_handler(ctx, new_pending_admin)
    }

    pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
        instructions::shared::fees::accept_admin_handler(ctx)
    }
}
