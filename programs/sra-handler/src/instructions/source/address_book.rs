use anchor_lang::prelude::*;
use sra_common::{
    errors::SraError,
    seeds::SEED_ADDRESS_BOOK,
    types::{AddressBookEntry, SraConfig},
};

use crate::events::AddressBookEntrySet;

/// Owner-authorized upsert for an SRA-scoped target-token mapping; any payer
/// may cover the rent.
///
/// This matches the EVM trust model: the `sra_config.owner` curates the
/// `(target_chain_id, target_token)` mapping for that SRA, analogous to each EVM
/// account pointing at its own owner-managed address book. `owner` must sign;
/// `payer` is independent and only funds the PDA allocation on first insert.
///
/// Upsert is safe: `init_if_needed` only allocates the PDA, and every stored
/// field is overwritten on every call, so no partial or stale state can survive
/// a rewrite.
///
/// A `target_decimals` value of `0` is a logical tombstone. That gives owners a
/// revoke path analogous to overwriting `ManagedAddressBook` entries with zeroed
/// `TokenInfo` on EVM, while keeping the PDA stable. Rent is never reclaimed —
/// tombstoned entries retain their PDA and their rent exemption. Downstream
/// consumers must gate on `AddressBookEntry::is_enabled()`.
#[derive(Accounts)]
#[instruction(target_chain_id: u64, target_token: [u8; 32])]
pub struct SetAddressBookEntry<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + AddressBookEntry::INIT_SPACE,
        seeds = [
            SEED_ADDRESS_BOOK,
            sra_config.key().as_ref(),
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
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Result<()> {
    require!(target_token != [0u8; 32], SraError::InvalidTokenInfo);

    let address_book_entry = &mut ctx.accounts.address_book_entry;
    address_book_entry.sra_config = ctx.accounts.sra_config.key();
    address_book_entry.target_chain_id = target_chain_id;
    address_book_entry.target_token = target_token;
    address_book_entry.target_decimals = target_decimals;
    address_book_entry.bump = ctx.bumps.address_book_entry;

    emit!(AddressBookEntrySet {
        sra_config: address_book_entry.sra_config,
        target_chain_id: address_book_entry.target_chain_id,
        target_token: address_book_entry.target_token,
        target_decimals: address_book_entry.target_decimals,
    });

    Ok(())
}
