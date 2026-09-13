use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use sra_common::{
    errors::SraError,
    fees::compute_protocol_fee,
    seeds::{
        SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_ASSET_FEE_TIER, SEED_FEE_CONFIG,
        SEED_PROTOCOL_FEE, SEED_SRA,
    },
    source_deposit::{decode_output_amount, output_satisfies_slippage_floor, TriggerArgs},
    types::{AddressBookEntry, AssetFeeTier, FeeConfig, ProtocolFeeConfig, SraConfig},
};

use crate::events::DepositTriggered;
use crate::utils::adapter::validate_adapter_program;
use crate::utils::ix_attestation::verify_next_adapter_ix;
use crate::utils::read_optional_program_account;

#[derive(Accounts)]
pub struct Trigger<'info> {
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    pub initiator: Signer<'info>,
    #[account(mut)]
    pub sra_config: Box<Account<'info, SraConfig>>,
    pub source_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: derived from the adapter program and SRA configuration.
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
        seeds::program = adapter_program.key(),
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = fee_payer,
        associated_token::mint = source_mint,
        associated_token::authority = adapter_authority,
    )]
    pub adapter_custody: Box<Account<'info, TokenAccount>>,
    /// CHECK: validated as an executable allowlisted adapter program.
    pub adapter_program: UncheckedAccount<'info>,
    #[account(
        seeds = [
            SEED_ADDRESS_BOOK,
            sra_config.handler_data.address_book_root.as_ref(),
            source_mint.key().as_ref(),
            &sra_config.handler_data.target_chain_id.to_le_bytes(),
            &sra_config.handler_data.target_token,
        ],
        bump = address_book_entry.bump,
    )]
    pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
    #[account(seeds = [SEED_FEE_CONFIG], bump = fee_config.bump)]
    pub fee_config: Box<Account<'info, FeeConfig>>,
    /// CHECK: singleton PDA; empty data means no protocol fee is configured.
    #[account(seeds = [SEED_PROTOCOL_FEE], bump)]
    pub protocol_fee_config: UncheckedAccount<'info>,
    /// CHECK: source-mint fee-tier PDA; empty data means use the default fee.
    #[account(seeds = [SEED_ASSET_FEE_TIER, source_mint.key().as_ref()], bump)]
    pub asset_fee_tier: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = fee_config.fee_recipient,
    )]
    pub fee_recipient_token_account: Option<Box<Account<'info, TokenAccount>>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: pinned to the instructions sysvar address.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<Trigger>, args: TriggerArgs) -> Result<()> {
    let TriggerArgs {
        params,
        paired_adapter_ix_data_hash,
    } = args;
    let sra_config = &ctx.accounts.sra_config;
    require!(sra_config.active, SraError::SraInactive);
    require_keys_eq!(
        ctx.accounts.initiator.key(),
        sra_config.initiator,
        SraError::InitiatorNotAllowed
    );
    require!(params.input_amount > 0, SraError::ZeroInputAmount);
    require!(
        ctx.accounts.sra_vault.amount >= params.input_amount,
        SraError::InsufficientSourceBalance
    );

    let config_hash = sra_config
        .handler_data
        .config_hash(&sra_config.owner, &sra_config.final_handler);
    require!(
        sra_config.handler_data.target_chain_id == params.target_chain_id
            && sra_config.handler_data.recipient == params.recipient
            && sra_config.handler_data.target_token == params.target_token,
        SraError::RouteMismatch
    );
    require!(
        ctx.accounts.address_book_entry.is_enabled(),
        SraError::InvalidAddressBook
    );

    validate_adapter_program(&ctx.accounts.adapter_program.to_account_info())?;
    let adapter_program = ctx.accounts.adapter_program.key();
    require!(
        sra_config
            .handler_data
            .allowed_adapters
            .contains(&adapter_program),
        SraError::AdapterNotAllowed
    );
    verify_next_adapter_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &adapter_program,
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.source_mint.key(),
        &paired_adapter_ix_data_hash,
    )?;

    let protocol_fee_config = read_optional_program_account::<ProtocolFeeConfig>(
        ctx.accounts.protocol_fee_config.to_account_info(),
    )?;
    let asset_fee_tier = read_optional_program_account::<AssetFeeTier>(
        ctx.accounts.asset_fee_tier.to_account_info(),
    )?;
    let expected_fee = compute_protocol_fee(
        params.is_sponsor,
        &ctx.accounts.fee_config.fee_recipient,
        protocol_fee_config.as_ref(),
        asset_fee_tier.as_ref(),
        params.input_amount,
        params.protocol_fee_bps_override,
    )?;
    require!(
        params.protocol_fee_amount == expected_fee,
        SraError::FeeMismatch
    );
    require!(
        output_satisfies_slippage_floor(
            params.input_amount,
            ctx.accounts.source_mint.decimals,
            decode_output_amount(params.output_amount)?,
            ctx.accounts.address_book_entry.target_decimals,
            sra_config.handler_data.allowed_slippage_bps,
        )?,
        SraError::SlippageExceeded
    );

    let nonce = sra_config.deposit_nonce;
    let next_nonce = nonce.checked_add(1).ok_or(SraError::ArithmeticOverflow)?;
    let owner = sra_config.owner.to_bytes();
    let salt = sra_config.salt;
    let bump = [sra_config.bump];
    let signer_seeds: &[&[u8]] = &[
        SEED_SRA,
        owner.as_ref(),
        config_hash.as_ref(),
        salt.as_ref(),
        bump.as_ref(),
    ];
    let signer: &[&[&[u8]]] = &[signer_seeds];

    if expected_fee > 0 {
        let fee_recipient_token_account = ctx
            .accounts
            .fee_recipient_token_account
            .as_ref()
            .ok_or(SraError::MissingFeeRecipientAccount)?;
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.sra_vault.to_account_info(),
                    mint: ctx.accounts.source_mint.to_account_info(),
                    to: fee_recipient_token_account.to_account_info(),
                    authority: ctx.accounts.sra_config.to_account_info(),
                },
                signer,
            ),
            expected_fee,
            ctx.accounts.source_mint.decimals,
        )?;
    }
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            TransferChecked {
                from: ctx.accounts.sra_vault.to_account_info(),
                mint: ctx.accounts.source_mint.to_account_info(),
                to: ctx.accounts.adapter_custody.to_account_info(),
                authority: ctx.accounts.sra_config.to_account_info(),
            },
            signer,
        ),
        params.net_input_amount()?,
        ctx.accounts.source_mint.decimals,
    )?;
    ctx.accounts.sra_config.deposit_nonce = next_nonce;

    emit!(DepositTriggered {
        sra_config: ctx.accounts.sra_config.key(),
        initiator: ctx.accounts.initiator.key(),
        source_mint: ctx.accounts.source_mint.key(),
        adapter_program,
        nonce,
        input_amount: params.input_amount,
        protocol_fee_amount: expected_fee,
        output_amount: params.output_amount,
        target_chain_id: params.target_chain_id,
        recipient: params.recipient,
        target_token: params.target_token,
    });
    Ok(())
}
