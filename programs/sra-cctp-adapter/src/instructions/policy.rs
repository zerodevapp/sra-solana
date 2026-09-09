use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
use sra_common::{
    constants::USDC_MINT,
    seeds::{config_hash, route_config_hash, SEED_ADDRESS_BOOK, SEED_SRA, SEED_SRA_ROUTE},
    types::{AddressBookEntry, AddressBookRoot, SolanaSraRoute, SourceHandlerData, SraConfig},
};

use super::execute_deposit::validate_finality_threshold;
use crate::{
    events::{DestinationPolicyInitialized, SourcePolicyInitialized, SourcePolicyUpdated},
    state::*,
    CctpAdapterError,
};

#[derive(Accounts)]
#[instruction(salt: [u8; 32])]
pub struct InitializeDestinationPolicy<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(has_one = owner,
        seeds = [SEED_SRA_ROUTE, owner.key().as_ref(), &route_config_hash(&handler_route.owner, handler_route.source_chain_id, &handler_route.expected_mint, &handler_route.action_config)?, &handler_route.salt],
        bump = handler_route.bump, seeds::program = sra_common::ID)]
    pub handler_route: Box<Account<'info, SolanaSraRoute>>,
    #[account(address = USDC_MINT, constraint = mint.decimals == 6 @ CctpAdapterError::InvalidPolicyConfiguration)]
    pub mint: Box<Account<'info, Mint>>,
    #[account(init, payer = payer, space = 8 + DestinationPolicy::INIT_SPACE,
        seeds = [SEED_DESTINATION_POLICY, handler_route.key().as_ref(), mint.key().as_ref(), &salt], bump)]
    pub destination_policy: Box<Account<'info, DestinationPolicy>>,
    #[account(init_if_needed, payer = payer, associated_token::mint = mint, associated_token::authority = destination_policy)]
    pub custody: Box<Account<'info, TokenAccount>>,
    #[account(init_if_needed, payer = payer, space = 8 + ExecutionLane::INIT_SPACE,
        seeds = [SEED_EXECUTION_LANE, sra_common::ID.as_ref(), handler_route.key().as_ref()], bump)]
    pub execution_lane: Box<Account<'info, ExecutionLane>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: The pinned sysvar proves fresh route creation before first enrollment.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
}

#[allow(clippy::too_many_arguments)]
pub fn initialize_destination_policy(
    ctx: Context<InitializeDestinationPolicy>,
    salt: [u8; 32],
    sender: [u8; 32],
    source_token: [u8; 32],
    finality: u32,
    max_fee: u64,
    min_burn: u64,
    max_burn: u64,
) -> Result<()> {
    validate_finality_threshold(finality)?;
    validate_bounds(min_burn, max_burn, max_fee)?;
    let route = &ctx.accounts.handler_route;
    require!(
        route.expected_mint == USDC_MINT
            && route.source_chain_id == MANAGED_CHAIN_ID
            && route.allowed_adapters == [crate::ID]
            && route.allowed_executors.len() == 1
            && route.allowed_executors[0] != Pubkey::default()
            && sender[..12] == [0; 12]
            && sender[12..] != [0; 20]
            && source_token == MANAGED_USDC,
        CctpAdapterError::InvalidPolicyConfiguration
    );
    let lane = &mut ctx.accounts.execution_lane;
    if lane.handler_program == Pubkey::default() {
        require_fresh_route(&ctx.accounts.instructions_sysvar, &route.key())?;
        lane.handler_program = sra_common::ID;
        lane.handler_route = route.key();
    } else {
        require!(
            lane.handler_program == sra_common::ID && lane.handler_route == route.key(),
            CctpAdapterError::InvalidExecutionLane
        );
    }
    ctx.accounts
        .destination_policy
        .set_inner(DestinationPolicy {
            handler_route: route.key(),
            mint: ctx.accounts.mint.key(),
            salt,
            sender,
            source_token,
            source_domain: MANAGED_CCTP_DOMAIN,
            finality,
            max_fee,
            min_burn,
            max_burn,
            queued_liability: 0,
            bump: ctx.bumps.destination_policy,
        });
    emit!(DestinationPolicyInitialized {
        destination_policy: ctx.accounts.destination_policy.key(),
        handler_route: route.key(),
        execution_lane: lane.key()
    });
    Ok(())
}

fn validate_bounds(min: u64, max: u64, fee: u64) -> Result<()> {
    require!(
        min > 0 && min <= max && fee < min,
        CctpAdapterError::InvalidPolicyConfiguration
    );
    Ok(())
}

fn require_fresh_route(sysvar: &AccountInfo, route: &Pubkey) -> Result<()> {
    let current = instructions::load_current_index_checked(sysvar)?;
    require_keys_eq!(
        instructions::load_instruction_at_checked(usize::from(current), sysvar)?.program_id,
        crate::ID,
        CctpAdapterError::FreshRouteRequired
    );
    let previous = current
        .checked_sub(1)
        .ok_or(error!(CctpAdapterError::FreshRouteRequired))?;
    let ix = instructions::load_instruction_at_checked(usize::from(previous), sysvar)?;
    let discriminator = sra_common::instruction_data::instruction_discriminator("create_route");
    require!(
        ix.program_id == sra_common::ID
            && ix.data.starts_with(&discriminator)
            && ix
                .accounts
                .get(2)
                .is_some_and(|account| account.pubkey == *route && account.is_writable),
        CctpAdapterError::FreshRouteRequired
    );
    Ok(())
}

#[derive(Accounts)]
#[instruction(handler_data: SourceHandlerData, salt: [u8; 32])]
pub struct InitializeSourcePolicy<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(
        has_one = owner,
        constraint = sra_config.final_handler == sra_common::ID @ CctpAdapterError::InvalidPolicyConfiguration,
        seeds = [SEED_SRA, owner.key().as_ref(), &config_hash(&sra_config.owner, &sra_config.final_handler, &sra_config.handler_data_hash), &sra_config.salt],
        bump = sra_config.bump,
        seeds::program = sra_common::ID,
    )]
    pub sra_config: Box<Account<'info, SraConfig>>,
    #[account(address = USDC_MINT, constraint = mint.decimals == 6 @ CctpAdapterError::InvalidPolicyConfiguration)]
    pub mint: Box<Account<'info, Mint>>,
    #[account(has_one = owner, constraint = address_book_root.key() == handler_data.address_book_root @ CctpAdapterError::InvalidPolicyConfiguration)]
    pub address_book_root: Box<Account<'info, AddressBookRoot>>,
    #[account(
        seeds = [SEED_ADDRESS_BOOK, address_book_root.key().as_ref(), mint.key().as_ref(), &handler_data.target_chain_id.to_le_bytes(), &handler_data.target_token],
        bump = address_book_entry.bump,
        seeds::program = sra_common::ID,
        has_one = address_book_root,
        constraint = address_book_entry.source_mint == mint.key()
            && address_book_entry.target_chain_id == handler_data.target_chain_id
            && address_book_entry.target_token == handler_data.target_token
            && address_book_entry.target_decimals == 6 @ CctpAdapterError::InvalidPolicyConfiguration,
    )]
    pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
    #[account(init, payer = payer, space = 8 + SourcePolicy::INIT_SPACE,
        seeds = [SEED_SOURCE_POLICY, sra_config.key().as_ref(), mint.key().as_ref(), &salt], bump)]
    pub source_policy: Box<Account<'info, SourcePolicy>>,
    #[account(init_if_needed, payer = payer, associated_token::mint = mint, associated_token::authority = source_policy)]
    pub custody: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn initialize_source_policy(
    ctx: Context<InitializeSourcePolicy>,
    handler_data: SourceHandlerData,
    salt: [u8; 32],
    executor: Pubkey,
    finality: u32,
    min_gross: u64,
    max_gross: u64,
    max_fee: u64,
) -> Result<()> {
    handler_data.validate()?;
    validate_finality_threshold(finality)?;
    validate_bounds(min_gross, max_gross, max_fee)?;
    require!(
        handler_data.hash() == ctx.accounts.sra_config.handler_data_hash
            && handler_data.allowed_adapters.contains(&crate::ID)
            && handler_data.target_chain_id == MANAGED_CHAIN_ID
            && handler_data.target_token == MANAGED_USDC
            && executor != Pubkey::default(),
        CctpAdapterError::InvalidPolicyConfiguration
    );
    ctx.accounts.source_policy.set_inner(SourcePolicy {
        sra_config: ctx.accounts.sra_config.key(),
        mint: ctx.accounts.mint.key(),
        salt,
        executor,
        paused: true,
        nonce: 0,
        finality,
        min_gross,
        max_gross,
        max_fee,
        bump: ctx.bumps.source_policy,
    });
    emit!(SourcePolicyInitialized {
        source_policy: ctx.accounts.source_policy.key(),
        sra_config: ctx.accounts.sra_config.key()
    });
    Ok(())
}

#[derive(Accounts)]
pub struct SetSourcePolicy<'info> {
    pub owner: Signer<'info>,
    #[account(has_one = owner,
        constraint = sra_config.final_handler == sra_common::ID @ CctpAdapterError::InvalidPolicyConfiguration,
        seeds = [SEED_SRA, owner.key().as_ref(), &config_hash(&sra_config.owner, &sra_config.final_handler, &sra_config.handler_data_hash), &sra_config.salt],
        bump = sra_config.bump, seeds::program = sra_common::ID)]
    pub sra_config: Box<Account<'info, SraConfig>>,
    #[account(mut, has_one = sra_config,
        seeds = [SEED_SOURCE_POLICY, sra_config.key().as_ref(), source_policy.mint.as_ref(), &source_policy.salt], bump = source_policy.bump)]
    pub source_policy: Box<Account<'info, SourcePolicy>>,
}

pub fn set_source_policy(
    ctx: Context<SetSourcePolicy>,
    expected_nonce: u64,
    executor: Pubkey,
    paused: bool,
) -> Result<()> {
    let policy = &mut ctx.accounts.source_policy;
    require!(
        policy.nonce == expected_nonce,
        CctpAdapterError::NonceMismatch
    );
    require!(
        executor != Pubkey::default(),
        CctpAdapterError::InvalidPolicyConfiguration
    );
    policy.nonce = policy
        .nonce
        .checked_add(1)
        .ok_or(error!(CctpAdapterError::ArithmeticOverflow))?;
    policy.executor = executor;
    policy.paused = paused;
    emit!(SourcePolicyUpdated {
        source_policy: policy.key(),
        nonce: policy.nonce
    });
    Ok(())
}
