use anchor_lang::prelude::*;
use sra_common::{
    errors::SraError,
    seeds::{SEED_ADDRESS_BOOK, SEED_FEE_CONFIG},
    types::{AddressBookEntry, FeeConfig},
};

#[derive(Accounts)]
#[instruction(adapter: Pubkey, mint: Pubkey, target_chain_id: u64, target_token: [u8; 32])]
pub struct RegisterAddressBookEntry<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        init,
        payer = payer,
        space = 8 + AddressBookEntry::INIT_SPACE,
        seeds = [
            SEED_ADDRESS_BOOK,
            adapter.as_ref(),
            mint.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token.as_ref(),
        ],
        bump,
    )]
    pub address_book_entry: Account<'info, AddressBookEntry>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(adapter: Pubkey, mint: Pubkey, target_chain_id: u64, target_token: [u8; 32])]
pub struct SetAddressBookEntryActive<'info> {
    pub admin: Signer<'info>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump, has_one = admin)]
    pub fee_config: Account<'info, FeeConfig>,
    #[account(
        mut,
        seeds = [
            SEED_ADDRESS_BOOK,
            adapter.as_ref(),
            mint.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token.as_ref(),
        ],
        bump,
    )]
    pub address_book_entry: Account<'info, AddressBookEntry>,
}

pub(crate) fn register_handler(
    ctx: Context<RegisterAddressBookEntry>,
    adapter: Pubkey,
    mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Result<()> {
    require!(
        target_token != [0u8; 32] && target_decimals != 0,
        SraError::InvalidTokenInfo
    );

    let address_book_entry = &mut ctx.accounts.address_book_entry;
    address_book_entry.adapter = adapter;
    address_book_entry.mint = mint;
    address_book_entry.target_chain_id = target_chain_id;
    address_book_entry.target_token = target_token;
    address_book_entry.target_decimals = target_decimals;
    address_book_entry.active = true;
    address_book_entry.bump = ctx.bumps.address_book_entry;
    Ok(())
}

// Args are unused in the body but required by the Accounts struct's
// #[instruction(...)] macro so Anchor can rebuild the PDA seeds.
pub(crate) fn set_active_handler(
    ctx: Context<SetAddressBookEntryActive>,
    _adapter: Pubkey,
    _mint: Pubkey,
    _target_chain_id: u64,
    _target_token: [u8; 32],
    active: bool,
) -> Result<()> {
    ctx.accounts.address_book_entry.active = active;
    Ok(())
}
