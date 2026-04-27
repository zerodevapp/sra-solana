use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};
use sra_common::{
    constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS},
    errors::SraError,
    ix_attestation::Routing,
    seeds::{config_hash, SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_SRA},
    types::{AddressBookEntry, MintNonce, SraConfig},
};

use crate::events::DepositPrepared;
use crate::utils::ix_attestation::verify_adapter_ix;

fn is_program_owner(owner: &Pubkey) -> bool {
    bpf_loader::check_id(owner)
        || bpf_loader_deprecated::check_id(owner)
        || bpf_loader_upgradeable::check_id(owner)
        || loader_v4::check_id(owner)
}

/// IX 1 inputs: how much the owner is locking (`input_amount`), the minimum
/// output they'll accept on the destination chain (`output_amount`), plus the
/// dynamic message fields
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct PrepareParams {
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
}

/// Source-side deposit IX. Validates the SRA + per-call routing inputs, pulls
/// the owner's tokens through the SRA vault into adapter custody, and emits
/// the raw routing inputs that IX 2 or an off-chain relayer needs to build a
/// target-specific bridge message.
///
/// **Not standalone.** This is the first instruction of a 2-IX versioned
/// transaction.
#[derive(Accounts)]
#[instruction(params: PrepareParams)]
pub struct ValidateAndPrepare<'info> {
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// Owns the source ATA and authorizes the user->vault transfer.
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Box<Account<'info, SraConfig>>,
    #[account(
        init_if_needed,
        payer = fee_payer,
        space = 8 + MintNonce::INIT_SPACE,
        seeds = [
            SEED_MINT_NONCE,
            sra_config.key().as_ref(),
            source_mint.key().as_ref(),
        ],
        bump,
    )]
    pub mint_nonce: Box<Account<'info, MintNonce>>,
    pub source_mint: Box<Account<'info, Mint>>,
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = owner,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,
    #[account(
        init_if_needed,
        payer = fee_payer,
        associated_token::mint = source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA `["adapter_authority", sra_config]` under `adapter_program`.
    /// Used as the adapter_custody ATA's authority; Anchor constrains both.
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
    /// CHECK: the handler enforces that this is an executable program id and a
    /// member of `sra_config.allowed_adapters`.
    pub adapter_program: UncheckedAccount<'info>,
    #[account(
        has_one = sra_config,
        seeds = [
            SEED_ADDRESS_BOOK,
            sra_config.key().as_ref(),
            &sra_config.target_chain_id.to_le_bytes(),
            &params.target_token,
        ],
        bump = address_book_entry.bump,
    )]
    pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: pinned to the Instructions sysvar. TODO we'll will read this for
    /// IX 2 attestation; the handler doesn't touch it yet.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
}

pub(crate) fn handler(ctx: Context<ValidateAndPrepare>, params: PrepareParams) -> Result<()> {
    require!(
        params.action_idxs.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );

    // Step 0a — adapter must be an executable program and be in the SRA's
    // allowlist.
    require!(
        !ctx.accounts.adapter_program.data_is_empty()
            && ctx.accounts.adapter_program.executable
            && is_program_owner(ctx.accounts.adapter_program.owner),
        SraError::InvalidAdapterProgram
    );
    let adapter_key = ctx.accounts.adapter_program.key();
    require!(
        ctx.accounts
            .sra_config
            .allowed_adapters
            .iter()
            .any(|a| a == &adapter_key),
        SraError::AdapterNotAllowed
    );

    // Step 0b — address book entry enabled. Its PDA seeds already pin it to
    // `(sra_config, target_chain_id, params.target_token)`, so only the
    // tombstone bit is left to check.
    require!(
        ctx.accounts.address_book_entry.is_enabled(),
        SraError::InvalidAddressBook
    );

    // Step 0c — IX 2 attestation. The next instruction must be the nominated
    // adapter's `execute_deposit_across` for this SRA, with byte-identical
    // routing fields. Without this check, an attacker could pair an honest
    // IX 1 (which moves the owner's tokens into adapter custody) with an
    // IX 2 that drains them along a different path or to a different SRA.
    let routing = Routing {
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        target_token: &params.target_token,
        action_idxs: &params.action_idxs,
    };
    verify_adapter_ix(
        &ctx.accounts.instructions_sysvar,
        &adapter_key,
        &ctx.accounts.sra_config.key(),
        &routing,
    )?;

    // Step 1 — adapter_authority PDA validation: enforced declaratively by
    // the `seeds` constraint on the Accounts struct.

    // Step 2 — transfer owner ATA → SRA vault. `transfer_checked` enforces
    // mint + decimals so we can't be fooled by a spoofed token account.
    {
        let cpi_accounts = TransferChecked {
            from: ctx.accounts.owner_token_account.to_account_info(),
            mint: ctx.accounts.source_mint.to_account_info(),
            to: ctx.accounts.sra_vault.to_account_info(),
            authority: ctx.accounts.owner.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        transfer_checked(
            cpi_ctx,
            params.input_amount,
            ctx.accounts.source_mint.decimals,
        )?;
    }

    // Step 3 — slippage. u128 lets u64-max inputs multiply cleanly.
    // `allowed_slippage_bps <= BPS_DENOMINATOR` is enforced on SraConfig
    // writes, so the subtraction cannot underflow.
    let slippage_bps = ctx.accounts.sra_config.allowed_slippage_bps as u128;
    let denom = BPS_DENOMINATOR as u128;
    let min_acceptable = (params.input_amount as u128)
        .checked_mul(denom - slippage_bps)
        .ok_or(SraError::ArithmeticOverflow)?
        / denom;
    require!(
        (params.output_amount as u128) >= min_acceptable,
        SraError::SlippageExceeded
    );

    // Step 4 — capture the pre-bump nonce.
    let nonce_now = ctx.accounts.mint_nonce.nonce;

    // Step 5 — transfer SRA vault → adapter custody, signed by the SRA PDA.
    {
        let ch = config_hash(
            &ctx.accounts.sra_config.owner,
            ctx.accounts.sra_config.target_chain_id,
            &ctx.accounts.sra_config.target_handler,
            &ctx.accounts.sra_config.target_adapter,
            &ctx.accounts.sra_config.recipient,
            &ctx.accounts.sra_config.app_id,
        );
        let owner_bytes = ctx.accounts.sra_config.owner.to_bytes();
        let salt = ctx.accounts.sra_config.salt;
        let bump_arr = [ctx.accounts.sra_config.bump];
        let seeds: &[&[u8]] = &[
            SEED_SRA,
            owner_bytes.as_ref(),
            ch.as_ref(),
            salt.as_ref(),
            bump_arr.as_ref(),
        ];
        let signer_seeds: &[&[&[u8]]] = &[seeds];

        let cpi_accounts = TransferChecked {
            from: ctx.accounts.sra_vault.to_account_info(),
            mint: ctx.accounts.source_mint.to_account_info(),
            to: ctx.accounts.adapter_custody.to_account_info(),
            authority: ctx.accounts.sra_config.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        transfer_checked(
            cpi_ctx,
            params.input_amount,
            ctx.accounts.source_mint.decimals,
        )?;
    }

    // Step 6 — bump the nonce so IX 2 can recover `nonce_now` as `nonce - 1`.
    // Stamp the PDA bump only on first init; `init_if_needed` zero-fills the
    // account, and a canonical PDA bump of zero is statistically impossible.
    let mint_nonce = &mut ctx.accounts.mint_nonce;
    if mint_nonce.bump == 0 {
        mint_nonce.bump = ctx.bumps.mint_nonce;
    }
    mint_nonce.nonce = mint_nonce
        .nonce
        .checked_add(1)
        .ok_or(SraError::ArithmeticOverflow)?;

    // Step 7 — emit the raw routing fields for off-chain observers.
    emit!(DepositPrepared {
        sra_config: ctx.accounts.sra_config.key(),
        source_mint: ctx.accounts.source_mint.key(),
        adapter_program: adapter_key,
        nonce: nonce_now,
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        target_chain_id: ctx.accounts.sra_config.target_chain_id,
        target_handler: ctx.accounts.sra_config.target_handler,
        target_adapter: ctx.accounts.sra_config.target_adapter,
        recipient: ctx.accounts.sra_config.recipient,
        app_id: ctx.accounts.sra_config.app_id,
        target_token: params.target_token,
        action_idxs: params.action_idxs,
    });

    Ok(())
}
