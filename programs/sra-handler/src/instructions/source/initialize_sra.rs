use anchor_lang::prelude::*;
use sra_common::{
    errors::SraError,
    seeds::{handler_data_hash, SEED_SRA},
    types::{AddressBookRoot, SourceHandlerData, SraConfig},
};

use crate::events::SraInitialized;

#[derive(Accounts)]
#[instruction(config_hash: [u8; 32], handler_data: SourceHandlerData, salt: [u8; 32])]
pub struct InitializeSra<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    // SRA owner must also own the address-book root — collapses trust to one
    // principal. Stricter than EVM (any IAddressBook); shared books deferred.
    #[account(
        constraint = address_book_root.owner == owner.key() @ SraError::InvalidAddressBook,
        constraint = handler_data.address_book_root == address_book_root.key() @ SraError::InvalidAddressBook,
    )]
    pub address_book_root: Account<'info, AddressBookRoot>,
    #[account(
        init,
        payer = payer,
        space = 8 + SraConfig::INIT_SPACE,
        seeds = [SEED_SRA, owner.key().as_ref(), &config_hash, &salt],
        bump,
    )]
    pub sra_config: Account<'info, SraConfig>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(
    ctx: Context<InitializeSra>,
    config_hash: [u8; 32],
    handler_data: SourceHandlerData,
    salt: [u8; 32],
) -> Result<()> {
    handler_data.validate()?;
    let final_handler = crate::ID;
    let expected_config_hash = handler_data.config_hash(&ctx.accounts.owner.key(), &final_handler);
    require!(
        expected_config_hash == config_hash,
        SraError::InvalidHandlerData
    );
    let handler_data_hash = handler_data_hash(&handler_data);

    let sra_config = &mut ctx.accounts.sra_config;
    sra_config.owner = ctx.accounts.owner.key();
    sra_config.final_handler = final_handler;
    sra_config.handler_data_hash = handler_data_hash;
    sra_config.salt = salt;
    sra_config.bump = ctx.bumps.sra_config;

    emit!(SraInitialized {
        sra_config: sra_config.key(),
        owner: sra_config.owner,
        final_handler: sra_config.final_handler,
        handler_data_hash: sra_config.handler_data_hash,
        salt: sra_config.salt,
    });

    Ok(())
}
