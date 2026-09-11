use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
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
    pub initial_source_mint: Box<Account<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = initial_source_mint,
        associated_token::authority = sra_config,
    )]
    pub initial_source_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

pub(crate) fn handler(
    ctx: Context<InitializeSra>,
    config_hash: [u8; 32],
    handler_data: SourceHandlerData,
    salt: [u8; 32],
    initiator: Pubkey,
) -> Result<()> {
    handler_data.validate()?;
    require_keys_neq!(initiator, Pubkey::default(), SraError::InvalidInitiator);
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
    sra_config.handler_data = handler_data;
    sra_config.salt = salt;
    sra_config.initiator = initiator;
    sra_config.active = true;
    sra_config.deposit_nonce = 0;
    sra_config.bump = ctx.bumps.sra_config;

    emit!(SraInitialized {
        sra_config: sra_config.key(),
        owner: sra_config.owner,
        final_handler: sra_config.final_handler,
        handler_data_hash,
        salt: sra_config.salt,
        initiator: sra_config.initiator,
        active: sra_config.active,
        deposit_nonce: sra_config.deposit_nonce,
        initial_source_mint: ctx.accounts.initial_source_mint.key(),
        initial_source_vault: ctx.accounts.initial_source_vault.key(),
    });

    Ok(())
}
