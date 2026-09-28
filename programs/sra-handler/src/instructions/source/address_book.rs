use anchor_lang::prelude::*;
use sra_common::{
    errors::SraError,
    seeds::SEED_ADDRESS_BOOK,
    types::{AddressBookEntry, AddressBookRoot},
};

use crate::events::{AddressBookEntrySet, AddressBookRootInitialized};

#[derive(Accounts)]
pub struct InitializeAddressBookRoot<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + AddressBookRoot::INIT_SPACE,
        constraint = address_book_root.key() != owner.key() @ SraError::InvalidAddressBook,
    )]
    pub address_book_root: Account<'info, AddressBookRoot>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn initialize_root_handler(ctx: Context<InitializeAddressBookRoot>) -> Result<()> {
    ctx.accounts.address_book_root.owner = ctx.accounts.owner.key();

    emit!(AddressBookRootInitialized {
        address_book_root: ctx.accounts.address_book_root.key(),
        owner: ctx.accounts.owner.key(),
    });

    Ok(())
}

/// Owner-authorized upsert for an address-book-root-scoped (source_mint, chain, target_token)
/// mapping; any payer may cover the rent.
#[derive(Accounts)]
#[instruction(source_mint: Pubkey, target_chain_id: u64, target_token: [u8; 32])]
pub struct SetAddressBookEntry<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub address_book_root: Account<'info, AddressBookRoot>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + AddressBookEntry::INIT_SPACE,
        seeds = [
            SEED_ADDRESS_BOOK,
            address_book_root.key().as_ref(),
            source_mint.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token.as_ref(),
        ],
        bump,
    )]
    pub address_book_entry: Account<'info, AddressBookEntry>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn set_entry_handler(
    ctx: Context<SetAddressBookEntry>,
    source_mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Result<()> {
    require!(target_token != [0u8; 32], SraError::InvalidTokenInfo);

    let address_book_entry = &mut ctx.accounts.address_book_entry;
    address_book_entry.address_book_root = ctx.accounts.address_book_root.key();
    address_book_entry.source_mint = source_mint;
    address_book_entry.target_chain_id = target_chain_id;
    address_book_entry.target_token = target_token;
    address_book_entry.target_decimals = target_decimals;
    address_book_entry.bump = ctx.bumps.address_book_entry;

    emit!(AddressBookEntrySet {
        address_book_root: address_book_entry.address_book_root,
        source_mint: address_book_entry.source_mint,
        target_chain_id: address_book_entry.target_chain_id,
        target_token: address_book_entry.target_token,
        target_decimals: address_book_entry.target_decimals,
    });

    Ok(())
}
