use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};
use solana_program::{
    bpf_loader_upgradeable::get_program_data_address,
    loader_v4::{LoaderV4State, LoaderV4Status},
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

fn deserialize_loader_v4_state(account: &AccountInfo) -> Result<LoaderV4State> {
    let data = account
        .try_borrow_data()
        .map_err(|_| error!(SraError::InvalidAdapterProgram))?;
    require!(
        data.len() >= LoaderV4State::program_data_offset(),
        SraError::InvalidAdapterProgram
    );

    let mut slot_bytes = [0u8; 8];
    slot_bytes.copy_from_slice(&data[0..8]);

    let mut authority_bytes = [0u8; 32];
    authority_bytes.copy_from_slice(&data[8..40]);

    let mut status_bytes = [0u8; 8];
    status_bytes.copy_from_slice(&data[40..48]);
    let status = match u64::from_le_bytes(status_bytes) {
        0 => LoaderV4Status::Retracted,
        1 => LoaderV4Status::Deployed,
        2 => LoaderV4Status::Finalized,
        _ => return Err(error!(SraError::InvalidAdapterProgram)),
    };

    Ok(LoaderV4State {
        slot: u64::from_le_bytes(slot_bytes),
        authority_address_or_next_version: Pubkey::new_from_array(authority_bytes),
        status,
    })
}

fn validate_immutable_adapter_program(
    adapter_program: &AccountInfo,
    adapter_program_data: &AccountInfo,
) -> Result<()> {
    require!(
        !adapter_program.data_is_empty() && adapter_program.executable,
        SraError::InvalidAdapterProgram
    );

    if bpf_loader::check_id(adapter_program.owner) || bpf_loader_deprecated::check_id(adapter_program.owner) {
        return Ok(());
    }

    if bpf_loader_upgradeable::check_id(adapter_program.owner) {
        require!(
            adapter_program_data.key() == get_program_data_address(adapter_program.key)
                && bpf_loader_upgradeable::check_id(adapter_program_data.owner),
            SraError::InvalidAdapterProgram
        );

        let data = adapter_program_data
            .try_borrow_data()
            .map_err(|_| error!(SraError::InvalidAdapterProgram))?;
        let programdata = ProgramData::try_deserialize(&mut data.as_ref())
            .map_err(|_| error!(SraError::InvalidAdapterProgram))?;
        return if programdata.upgrade_authority_address.is_none() {
            Ok(())
        } else {
            Err(error!(SraError::InvalidAdapterProgram))
        };
    }

    if loader_v4::check_id(adapter_program.owner) {
        let state = deserialize_loader_v4_state(adapter_program)?;
        require!(
            matches!(state.status, LoaderV4Status::Finalized),
            SraError::InvalidAdapterProgram
        );
        return Ok(());
    }

    Err(error!(SraError::InvalidAdapterProgram))
}

pub use sra_common::source_deposit::PrepareParams;

/// Source-side deposit IX. Validates the SRA + per-call routing inputs, pulls
/// the owner's tokens through the SRA vault into adapter custody, and emits
/// the raw routing inputs that IX 2 or an off-chain relayer needs to build a
/// target-specific bridge message.
///
/// **Not standalone.** This is the first instruction of a 2-IX versioned
/// transaction.
///
/// Every Anchor `Account<'info, T>` is boxed: the `SourceHandlerData`
/// IX arg + three `TokenAccount`s push the unboxed struct ~100 bytes
/// over the 4 KB stack budget.
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
    /// CHECK: only used when `adapter_program` is owned by the upgradeable loader.
    pub adapter_program_data: UncheckedAccount<'info>,
    // See `initialize_sra` for ownership-coupling rationale.
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
    /// CHECK: pinned to the Instructions sysvar; read by `verify_next_adapter_ix`
    /// to bind this IX to its paired adapter `execute_deposit_across`.
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

    // Step 0a — adapter must be an executable program and be in the SRA's
    // allowlist.
    validate_immutable_adapter_program(
        &ctx.accounts.adapter_program.to_account_info(),
        &ctx.accounts.adapter_program_data.to_account_info(),
    )?;
    let adapter_key = ctx.accounts.adapter_program.key();
    require!(
        handler_data
            .allowed_adapters
            .iter()
            .any(|a| a == &adapter_key),
        SraError::AdapterNotAllowed
    );

    // Step 0b — entry seeds already pin (address_book_root, target_chain_id,
    // target_token); only the tombstone bit is left to check.
    require!(
        ctx.accounts.address_book_entry.is_enabled(),
        SraError::InvalidAddressBook
    );

    // Step 0c — IX 2 attestation: the immediately following instruction must
    // be the paired adapter IX with the pinned accounts and exact data hash the
    // client attested to. Adapter-specific semantics are verified by IX 2.
    verify_next_adapter_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &adapter_key,
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.source_mint.key(),
        &paired_adapter_ix_data_hash,
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
    let slippage_bps = handler_data.allowed_slippage_bps as u128;
    let denom = BPS_DENOMINATOR as u128;
    let min_acceptable = (params.input_amount as u128)
        .checked_mul(denom - slippage_bps)
        .ok_or(SraError::ArithmeticOverflow)?
        / denom;
    require!(
        (params.output_amount as u128) >= min_acceptable,
        SraError::SlippageExceeded
    );

    // Step 4 — capture the pre-bump nonce. Target-specific adapters can use
    // this plus the raw bytes32 routing fields from the event to derive their
    // own message shape later without forcing EVM address semantics here.
    let nonce_now = ctx.accounts.mint_nonce.nonce;

    // Step 5 — transfer SRA vault → adapter custody, signed by the SRA PDA.
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
    use anchor_lang::error;

    fn loader_v4_state_bytes(status: LoaderV4Status, authority: Pubkey) -> Vec<u8> {
        let mut data = Vec::with_capacity(LoaderV4State::program_data_offset());
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(authority.as_ref());
        let status_value = match status {
            LoaderV4Status::Retracted => 0u64,
            LoaderV4Status::Deployed => 1u64,
            LoaderV4Status::Finalized => 2u64,
        };
        data.extend_from_slice(&status_value.to_le_bytes());
        data
    }

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
    fn validate_immutable_adapter_program_accepts_legacy_loader() {
        let mut program = TestAccount {
            key: Pubkey::new_unique(),
            owner: bpf_loader::ID,
            lamports: 0,
            data: vec![1],
            executable: true,
        };
        let mut ignored = TestAccount {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 0,
            data: vec![],
            executable: false,
        };

        validate_immutable_adapter_program(&program.account_info(), &ignored.account_info())
            .expect("legacy loader program should be treated as immutable");
    }

    #[test]
    fn validate_immutable_adapter_program_rejects_upgradeable_program_with_authority() {
        let program_key = Pubkey::new_unique();
        let programdata_key = get_program_data_address(&program_key);
        let programdata_state = anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState::ProgramData {
            slot: 0,
            upgrade_authority_address: Some(Pubkey::new_unique()),
        };

        let mut program = TestAccount {
            key: program_key,
            owner: bpf_loader_upgradeable::ID,
            lamports: 0,
            data: vec![1],
            executable: true,
        };
        let mut programdata = TestAccount {
            key: programdata_key,
            owner: bpf_loader_upgradeable::ID,
            lamports: 0,
            data: bincode::serialize(&programdata_state).unwrap(),
            executable: false,
        };

        let err = validate_immutable_adapter_program(&program.account_info(), &programdata.account_info())
            .expect_err("upgrade authority must make the adapter invalid");
        assert_eq!(err, error!(SraError::InvalidAdapterProgram));
    }

    #[test]
    fn validate_immutable_adapter_program_rejects_wrong_upgradeable_programdata_account() {
        let program_key = Pubkey::new_unique();
        let wrong_programdata_key = Pubkey::new_unique();
        let programdata_state = anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState::ProgramData {
            slot: 0,
            upgrade_authority_address: None,
        };

        let mut program = TestAccount {
            key: program_key,
            owner: bpf_loader_upgradeable::ID,
            lamports: 0,
            data: vec![1],
            executable: true,
        };
        let mut wrong_programdata = TestAccount {
            key: wrong_programdata_key,
            owner: bpf_loader_upgradeable::ID,
            lamports: 0,
            data: bincode::serialize(&programdata_state).unwrap(),
            executable: false,
        };

        let err = validate_immutable_adapter_program(
            &program.account_info(),
            &wrong_programdata.account_info(),
        )
        .expect_err("wrong programdata PDA must reject");
        assert_eq!(err, error!(SraError::InvalidAdapterProgram));
    }

    #[test]
    fn validate_immutable_adapter_program_accepts_finalized_loader_v4_program() {
        let state = LoaderV4State {
            slot: 0,
            authority_address_or_next_version: Pubkey::default(),
            status: LoaderV4Status::Finalized,
        };
        let mut program = TestAccount {
            key: Pubkey::new_unique(),
            owner: loader_v4::ID,
            lamports: 0,
            data: loader_v4_state_bytes(state.status, state.authority_address_or_next_version),
            executable: true,
        };
        let mut ignored = TestAccount {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 0,
            data: vec![],
            executable: false,
        };

        validate_immutable_adapter_program(&program.account_info(), &ignored.account_info())
            .expect("finalized loader-v4 program should be accepted");
    }

    #[test]
    fn validate_immutable_adapter_program_rejects_non_finalized_loader_v4_program() {
        let state = LoaderV4State {
            slot: 0,
            authority_address_or_next_version: Pubkey::new_unique(),
            status: LoaderV4Status::Deployed,
        };
        let mut program = TestAccount {
            key: Pubkey::new_unique(),
            owner: loader_v4::ID,
            lamports: 0,
            data: loader_v4_state_bytes(state.status, state.authority_address_or_next_version),
            executable: true,
        };
        let mut ignored = TestAccount {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 0,
            data: vec![],
            executable: false,
        };

        let err = validate_immutable_adapter_program(&program.account_info(), &ignored.account_info())
            .expect_err("non-finalized loader-v4 program must reject");
        assert_eq!(err, error!(SraError::InvalidAdapterProgram));
    }
}
