use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};
use sra_common::{
    constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS},
    errors::SraError,
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_SRA},
    types::{AddressBookEntry, AddressBookRoot, MintNonce, SourceHandlerData, SraConfig},
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
    let scaled_input = (input_amount as u128)
        .checked_mul(slippage_numerator)
        .ok_or(SraError::ArithmeticOverflow)?;

    if input_decimals >= output_decimals {
        let min_acceptable = scaled_input / denom;
        let mut scaled_output = output_amount as u128;

        for _ in 0..(input_decimals - output_decimals) {
            if scaled_output >= min_acceptable {
                return Ok(true);
            }
            scaled_output *= 10;
        }

        return Ok(scaled_output >= min_acceptable);
    }

    let output = output_amount as u128;
    let mut min_acceptable = scaled_input / denom;
    let mut remainder = scaled_input % denom;

    for _ in 0..(output_decimals - input_decimals) {
        if min_acceptable > output / 10 {
            return Ok(false);
        }

        min_acceptable *= 10;
        let next_remainder = remainder * 10;
        min_acceptable += next_remainder / denom;
        remainder = next_remainder % denom;

        if min_acceptable > output {
            return Ok(false);
        }
    }

    Ok(output >= min_acceptable)
}

pub use sra_common::source_deposit::PrepareParams;

/// Validates a source deposit, moves funds into adapter custody, and emits the
/// routing inputs consumed by the paired adapter instruction.
#[derive(Accounts)]
#[instruction(_handler_data: SourceHandlerData, params: PrepareParams)]
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
            &params.target_chain_id.to_le_bytes(),
            &params.target_token,
        ],
        bump = address_book_entry.bump,
    )]
    pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: Instructions sysvar used to verify the next adapter instruction.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
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

    // Compare the decimal-normalized slippage floor without scaling into overflowing intermediates.
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

    // Transfer from the SRA vault into adapter custody.
    {
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
}
