use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};
use sra_common::{
    constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS},
    errors::SraError,
    fees::compute_protocol_fee,
    seeds::{
        SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_ASSET_FEE_TIER, SEED_FEE_CONFIG,
        SEED_MINT_NONCE, SEED_PROTOCOL_FEE, SEED_SRA,
    },
    types::{
        AddressBookEntry, AddressBookRoot, AssetFeeTier, FeeConfig, MintNonce, ProtocolFeeConfig,
        SourceHandlerData, SraConfig,
    },
};

use crate::events::DepositPrepared;
use crate::utils::ix_attestation::verify_next_adapter_ix;

fn is_program_owner(owner: &Pubkey) -> bool {
    bpf_loader::check_id(owner)
        || bpf_loader_deprecated::check_id(owner)
        || bpf_loader_upgradeable::check_id(owner)
        || loader_v4::check_id(owner)
}

fn validate_adapter_program(adapter_program: &AccountInfo) -> Result<()> {
    require!(
        !adapter_program.data_is_empty() && adapter_program.executable,
        SraError::InvalidAdapterProgram
    );
    require!(
        is_program_owner(adapter_program.owner),
        SraError::InvalidAdapterProgram
    );
    Ok(())
}

fn output_satisfies_slippage_floor(
    input_amount: u64,
    input_decimals: u8,
    output_amount: u64,
    output_decimals: u8,
    slippage_bps: u16,
) -> Result<bool> {
    let denom = BPS_DENOMINATOR as u128;
    let slippage_numerator = (BPS_DENOMINATOR - slippage_bps) as u128;
    let min_source = (input_amount as u128)
        .checked_mul(slippage_numerator)
        .ok_or(SraError::ArithmeticOverflow)?
        .div_ceil(denom);

    // Put both sides on the same decimal scale by multiplying the
    // lower-decimal side up by 10^|Δ|, then compare once. `output * 10^k >= min`
    // is exactly EVM's double-ceil (`output >= ceil(min / 10^k)`) for integers.
    let (lhs, rhs) = if input_decimals >= output_decimals {
        let k = (input_decimals - output_decimals) as u32;
        (
            (output_amount as u128).saturating_mul(10u128.saturating_pow(k)),
            min_source,
        )
    } else {
        let k = (output_decimals - input_decimals) as u32;
        (
            output_amount as u128,
            min_source.saturating_mul(10u128.saturating_pow(k)),
        )
    };

    Ok(lhs >= rhs)
}

pub use sra_common::source_deposit::PrepareParams;

/// Validates a source deposit, collects the protocol fee, moves the net into
/// adapter custody, and emits the routing inputs consumed by the paired
/// adapter instruction.
#[derive(Accounts)]
#[instruction(handler_data: SourceHandlerData, params: PrepareParams)]
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
    /// CHECK: adapter authority PDA under `adapter_program`; Anchor validates
    /// the derivation.
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
    /// CHECK: must be an executable allowlisted adapter program.
    pub adapter_program: UncheckedAccount<'info>,
    #[account(
        has_one = owner,
    )]
    pub address_book_root: Box<Account<'info, AddressBookRoot>>,
    #[account(
        seeds = [
            SEED_ADDRESS_BOOK,
            address_book_root.key().as_ref(),
            source_mint.key().as_ref(),
            &params.target_chain_id.to_le_bytes(),
            &params.target_token,
        ],
        bump = address_book_entry.bump,
    )]
    pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
    #[account(
        seeds = [SEED_FEE_CONFIG],
        bump = fee_config.bump,
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,
    /// CHECK: PDA ["protocol_fee", handler_data.app_id] at the canonical bump;
    /// empty data == no fee for this app — existence is read in-body, never
    /// caller-claimed.
    #[account(
        seeds = [SEED_PROTOCOL_FEE, handler_data.app_id.as_ref()],
        bump,
    )]
    pub protocol_fee_config: UncheckedAccount<'info>,
    /// CHECK: PDA ["asset_fee_tier", handler_data.app_id, source_mint] at the
    /// canonical bump; empty data == no tier, fall back to `default_fee_bps`.
    #[account(
        seeds = [
            SEED_ASSET_FEE_TIER,
            handler_data.app_id.as_ref(),
            source_mint.key().as_ref(),
        ],
        bump,
    )]
    pub asset_fee_tier: UncheckedAccount<'info>,
    /// Canonical ATA(fee_config.fee_recipient, source_mint)
    #[account(
        mut,
        associated_token::mint = source_mint,
        associated_token::authority = fee_config.fee_recipient,
    )]
    pub fee_recipient_token_account: Option<Box<Account<'info, TokenAccount>>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: Instructions sysvar used to verify the next adapter instruction.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
}

/// Reads a maybe-uninitialized, seeds-pinned program-owned PDA. Empty data ==
/// not configured (EVM `isSet == false`); otherwise apply the same owner +
/// discriminator + deserialize checks `Account::try_from` would — hand-rolled
/// because `try_from` needs a `&'info AccountInfo<'info>` we can't produce from
/// a derived (temporary) account info.
fn read_optional_program_account<T: AccountDeserialize + Owner>(
    info: AccountInfo,
) -> Result<Option<T>> {
    if info.data_is_empty() {
        return Ok(None);
    }
    require_keys_eq!(
        *info.owner,
        T::owner(),
        ErrorCode::AccountOwnedByWrongProgram
    );
    let data = info.try_borrow_data()?;
    T::try_deserialize(&mut data.as_ref()).map(Some)
}

pub(crate) fn handler(
    ctx: Context<ValidateAndPrepare>,
    handler_data: SourceHandlerData,
    params: PrepareParams,
    paired_adapter_ix_data_hash: [u8; 32],
) -> Result<()> {
    require!(
        params.action_idxs.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );
    // Reject zero-amount deposits up front: a zero `input_amount` would no-op the
    // transfer, clear the slippage floor, burn a nonce, and emit a phantom
    // DepositPrepared. Fail-closed before any hashing, attestation, or funds move.
    require!(params.input_amount > 0, SraError::ZeroInputAmount);
    handler_data.validate()?;
    require!(
        ctx.accounts.sra_config.final_handler == crate::ID,
        SraError::InvalidHandlerData
    );
    require!(
        handler_data.hash() == ctx.accounts.sra_config.handler_data_hash,
        SraError::InvalidHandlerData
    );
    require!(
        handler_data.address_book_root == ctx.accounts.address_book_root.key(),
        SraError::InvalidAddressBook
    );

    require!(
        handler_data.target_chain_id == params.target_chain_id
            && handler_data.target_handler == params.target_handler
            && handler_data.target_adapter == params.target_adapter
            && handler_data.recipient == params.recipient
            && handler_data.app_id == params.app_id
            && handler_data.target_token == params.target_token,
        SraError::RouteMismatch
    );

    validate_adapter_program(&ctx.accounts.adapter_program.to_account_info())?;
    let adapter_key = ctx.accounts.adapter_program.key();
    require!(
        handler_data
            .allowed_adapters
            .iter()
            .any(|a| a == &adapter_key),
        SraError::AdapterNotAllowed
    );
    require!(
        ctx.accounts.address_book_entry.is_enabled(),
        SraError::InvalidAddressBook
    );

    // Verify the next instruction is the paired adapter call.
    verify_next_adapter_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &adapter_key,
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.source_mint.key(),
        &paired_adapter_ix_data_hash,
    )?;

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

    // The SRA PDA signs both the fee leg and the custody leg below.
    let ch = handler_data.config_hash(
        &ctx.accounts.sra_config.owner,
        &ctx.accounts.sra_config.final_handler,
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

    let transfer_from_vault = |to, amount: u64| -> Result<()> {
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.sra_vault.to_account_info(),
                    mint: ctx.accounts.source_mint.to_account_info(),
                    to,
                    authority: ctx.accounts.sra_config.to_account_info(),
                },
                signer_seeds,
            ),
            amount,
            ctx.accounts.source_mint.decimals,
        )
    };

    // Recompute protocol fee from live admin state and require attested `protocol_fee_amount`.
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
    if expected_fee > 0 {
        let fee_recipient_token_account = ctx
            .accounts
            .fee_recipient_token_account
            .as_ref()
            .ok_or(SraError::MissingFeeRecipientAccount)?;
        transfer_from_vault(fee_recipient_token_account.to_account_info(), expected_fee)?;
    }
    let bridged_amount = params.net_input_amount()?;

    // Compare the decimal-normalized slippage floor without scaling into
    // overflowing intermediates.
    require!(
        output_satisfies_slippage_floor(
            params.input_amount,
            ctx.accounts.source_mint.decimals,
            params.output_amount,
            ctx.accounts.address_book_entry.target_decimals,
            handler_data.allowed_slippage_bps,
        )?,
        SraError::SlippageExceeded
    );

    let nonce_now = ctx.accounts.mint_nonce.nonce;

    // Transfer the net from the SRA vault into adapter custody.
    transfer_from_vault(
        ctx.accounts.adapter_custody.to_account_info(),
        bridged_amount,
    )?;

    // Persist the PDA bump on first init, then advance the nonce.
    let mint_nonce = &mut ctx.accounts.mint_nonce;
    if mint_nonce.bump == 0 {
        mint_nonce.bump = ctx.bumps.mint_nonce;
    }
    mint_nonce.nonce = mint_nonce
        .nonce
        .checked_add(1)
        .ok_or(SraError::ArithmeticOverflow)?;

    emit!(DepositPrepared {
        sra_config: ctx.accounts.sra_config.key(),
        source_mint: ctx.accounts.source_mint.key(),
        adapter_program: adapter_key,
        nonce: nonce_now,
        input_amount: params.input_amount,
        protocol_fee_amount: expected_fee,
        output_amount: params.output_amount,
        target_chain_id: params.target_chain_id,
        target_handler: params.target_handler,
        target_adapter: params.target_adapter,
        recipient: params.recipient,
        app_id: params.app_id,
        target_token: params.target_token,
        action_idxs: params.action_idxs,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAccount {
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Vec<u8>,
        executable: bool,
    }

    impl TestAccount {
        fn account_info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(
                &self.key,
                false,
                self.executable,
                &mut self.lamports,
                &mut self.data,
                &self.owner,
                self.executable,
                0,
            )
        }
    }

    #[test]
    fn validate_adapter_program_accepts_supported_loader_owners() {
        let cases = [
            ("legacy", bpf_loader::ID),
            ("upgradeable", bpf_loader_upgradeable::ID),
            ("loader_v4", loader_v4::ID),
        ];

        for (name, owner) in cases {
            let mut program = TestAccount {
                key: Pubkey::new_unique(),
                owner,
                lamports: 0,
                data: vec![1],
                executable: true,
            };

            validate_adapter_program(&program.account_info())
                .unwrap_or_else(|_| panic!("{name} loader-owned program should be accepted"));
        }
    }

    //  The slippage minimum must round UP at every step

    #[test]
    fn slippage_floor_rounds_up_equal_decimals() {
        // 1_000_001 * 9_950 = 9_950_009_950 → /10^4 = 995_000.995
        // EVM minimum = ceil = 995_001; a floored bound would accept 995_000.
        assert!(!output_satisfies_slippage_floor(1_000_001, 6, 995_000, 6, 50).unwrap());
        assert!(output_satisfies_slippage_floor(1_000_001, 6, 995_001, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_rounds_up_on_upscale() {
        // Same product; min = ceil(995_000.995) * 10 = 9_950_010 in 7dp output
        // units. A floor(N·10/10^4) bound would accept 9_950_009.
        assert!(!output_satisfies_slippage_floor(1_000_001, 6, 9_950_009, 7, 50).unwrap());
        assert!(output_satisfies_slippage_floor(1_000_001, 6, 9_950_010, 7, 50).unwrap());
    }

    #[test]
    fn slippage_floor_rounds_up_on_downscale() {
        // 10_051 * 9_950 = 100_007_450 → /10^4 = 10_000.745 → ceil 10_001 in
        // source (7dp) units. Output 1_000 (6dp) upscales to 10_000 — under the
        // ceil'd minimum, over a floored one.
        assert!(!output_satisfies_slippage_floor(10_051, 7, 1_000, 6, 50).unwrap());
        assert!(output_satisfies_slippage_floor(10_051, 7, 1_001, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_exact_division_boundary_unchanged() {
        // 1_000_000 * 9_950 / 10_000 = 995_000 exactly — floor == ceil; the
        // historical boundary behavior stays.
        assert!(output_satisfies_slippage_floor(1_000_000, 6, 995_000, 6, 50).unwrap());
        assert!(!output_satisfies_slippage_floor(1_000_000, 6, 994_999, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_zero_slippage_requires_full_amount() {
        assert!(output_satisfies_slippage_floor(1_000_000, 6, 1_000_000, 6, 0).unwrap());
        assert!(!output_satisfies_slippage_floor(1_000_000, 6, 999_999, 6, 0).unwrap());
    }

    #[test]
    fn slippage_floor_extreme_decimal_gap_saturates_correctly() {
        // 40-decimal gap: 10^40 overflows u128, so the scaled side saturates.
        // The clamp must stay on the correct side of the comparison.

        // Scale output UP (input finer): a non-zero output 40 decimals below
        // the input is astronomically more than the minimum once normalized,
        // so it clears — the saturated u128::MAX still satisfies `>= min`.
        assert!(output_satisfies_slippage_floor(1, 40, 1, 0, 50).unwrap());

        // Scale the minimum UP (output finer): the minimum normalized 40
        // decimals above the input dwarfs any u64 output, so it cannot clear —
        // the saturated minimum stays above `output`.
        assert!(!output_satisfies_slippage_floor(1_000_000, 0, u64::MAX, 40, 50).unwrap());

        // A zero output never clears a positive minimum, even under saturation.
        assert!(!output_satisfies_slippage_floor(1, 0, 0, 40, 50).unwrap());
    }
}
