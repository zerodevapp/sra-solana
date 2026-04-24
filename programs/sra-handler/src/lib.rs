use anchor_lang::prelude::*;

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

    pub fn register_address_book_entry(
        ctx: Context<RegisterAddressBookEntry>,
        adapter: Pubkey,
        mint: Pubkey,
        target_chain_id: u64,
        target_token: [u8; 32],
        target_decimals: u8,
    ) -> Result<()> {
        instructions::source::address_book::register_handler(
            ctx,
            adapter,
            mint,
            target_chain_id,
            target_token,
            target_decimals,
        )
    }

    pub fn set_address_book_entry_active(
        ctx: Context<SetAddressBookEntryActive>,
        adapter: Pubkey,
        mint: Pubkey,
        target_chain_id: u64,
        target_token: [u8; 32],
        active: bool,
    ) -> Result<()> {
        instructions::source::address_book::set_active_handler(
            ctx,
            adapter,
            mint,
            target_chain_id,
            target_token,
            active,
        )
    }
}
