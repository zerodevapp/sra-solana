use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};
use sra_common::{
    abi::{
        abi_encode_sra_message, build_extra_envelope, bytes32_to_evm_address, source_token_hash,
        u256_from_u64, DepositInfo, Message, TokenParam,
    },
    constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS, SOLANA_CHAIN_ID},
    errors::SraError,
    seeds::{config_hash, SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_SRA},
    types::{AddressBookEntry, MintNonce, SraConfig},
};

use crate::events::DepositPrepared;

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
/// the canonical ABI-encoded message bytes that IX 2 (`execute_deposit_across`)
/// will re-derive before CPI'ing into Across.
///
/// **Not standalone.** This is the first instruction of a 2-IX versioned
/// transaction. Step 13 adds IX 2 attestation via the `instructions_sysvar`
/// account — it's included here so the account layout (and therefore Step 13's
/// `handler_ix_layout` index constants) is stable across both commits.
///
/// Every Anchor `Account<'info, T>` is boxed to keep Anchor-generated
/// `try_accounts` within the 4 KB on-chain stack budget. With 8 accounts
/// (including `SraConfig`'s variable-sized `allowed_adapters` Vec and three
/// `TokenAccount`s) the unboxed struct overflows by ~100 bytes.
#[derive(Accounts)]
#[instruction(params: PrepareParams)]
pub struct ValidateAndPrepare<'info> {
    /// Pays tx fees. Separated from `owner` to enable gas sponsorship.
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
    /// CHECK: pinned to the Instructions sysvar. Step 13 will read this for
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
    // allowlist. This prevents a mistyped Pubkey from becoming custody owner.
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

    // Step 0c — IX 2 attestation: wired in Step 13.

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

    // Step 4 — build the canonical ABI message using the *current* (pre-bump)
    // nonce. IX 2 will read the (already bumped) nonce, subtract 1, and
    // reproduce this same byte sequence.
    let target_adapter_evm = bytes32_to_evm_address(&ctx.accounts.sra_config.target_adapter)
        .map_err(|_| error!(SraError::InvalidEvmAddress))?;
    let target_handler_evm = bytes32_to_evm_address(&ctx.accounts.sra_config.target_handler)
        .map_err(|_| error!(SraError::InvalidEvmAddress))?;
    let target_token_evm = bytes32_to_evm_address(&params.target_token)
        .map_err(|_| error!(SraError::InvalidEvmAddress))?;

    let nonce_now = ctx.accounts.mint_nonce.nonce;
    let extra = build_extra_envelope(&target_adapter_evm, nonce_now);

    let action_idxs: Vec<[u8; 32]> = params
        .action_idxs
        .iter()
        .map(|&i| u256_from_u64(i))
        .collect();

    let canonical_msg = Message {
        account: ctx.accounts.sra_config.recipient,
        init_code: Vec::new(),
        deposit_info: DepositInfo {
            deposit_param: TokenParam {
                token: source_token_hash(&ctx.accounts.source_mint.key()),
                amount: u256_from_u64(params.input_amount),
            },
            target_chain_id: u256_from_u64(ctx.accounts.sra_config.target_chain_id),
            target_param: TokenParam {
                token: target_token_evm,
                amount: u256_from_u64(params.output_amount),
            },
            extra,
        },
        action_idxs,
        app_id: ctx.accounts.sra_config.app_id,
    };
    let source_chain_id_bytes = u256_from_u64(SOLANA_CHAIN_ID);
    let encoded_message = abi_encode_sra_message(
        &target_adapter_evm,
        &target_handler_evm,
        &source_chain_id_bytes,
        &canonical_msg,
    );

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
    let mint_nonce = &mut ctx.accounts.mint_nonce;
    mint_nonce.bump = ctx.bumps.mint_nonce;
    mint_nonce.nonce = mint_nonce
        .nonce
        .checked_add(1)
        .ok_or(SraError::ArithmeticOverflow)?;

    // Step 7 — emit the canonical message bytes for off-chain observers.
    emit!(DepositPrepared {
        sra_config: ctx.accounts.sra_config.key(),
        source_mint: ctx.accounts.source_mint.key(),
        adapter_program: adapter_key,
        nonce: nonce_now,
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        message: encoded_message,
    });

    Ok(())
}
