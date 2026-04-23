use anchor_lang::prelude::*;
use sra_common::{
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
pub struct DeactivateAddressBookEntry<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
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
) -> Result<()> {
    let address_book_entry = &mut ctx.accounts.address_book_entry;
    address_book_entry.adapter = adapter;
    address_book_entry.mint = mint;
    address_book_entry.target_chain_id = target_chain_id;
    address_book_entry.target_token = target_token;
    address_book_entry.active = true;
    address_book_entry.bump = ctx.bumps.address_book_entry;
    Ok(())
}

pub(crate) fn deactivate_handler(
    ctx: Context<DeactivateAddressBookEntry>,
    _adapter: Pubkey,
    _mint: Pubkey,
    _target_chain_id: u64,
    _target_token: [u8; 32],
) -> Result<()> {
    ctx.accounts.address_book_entry.active = false;
    Ok(())
}
