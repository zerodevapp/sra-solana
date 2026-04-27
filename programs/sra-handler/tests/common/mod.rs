// Each integration-test binary (initialize_sra, update_adapters, ...) pulls in this module
// but only uses a subset of its helpers. Silence per-binary dead_code warnings on the shared set.
#![allow(dead_code)]
// IX builders mirror the on-chain accounts + args shape, which exceeds clippy's default.
#![allow(clippy::too_many_arguments)]

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use anchor_spl::{
    associated_token::{
        get_associated_token_address, spl_associated_token_account, ID as ATA_PROGRAM_ID,
    },
    token::{
        spl_token::{self, state::Mint as SplMint},
        ID as TOKEN_PROGRAM_ID,
    },
};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction,
    message::Message,
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::{instruction as system_instruction, program as system_program};
use sra_common::{
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_SRA},
    types::{AddressBookEntry, MintNonce, SraConfig},
};
use sra_handler::instructions::source::{
    initialize_sra::SraInitConfig, validate_and_prepare::PrepareParams,
};
use std::{fs, path::PathBuf};

pub const PROGRAM_ID: Pubkey = sra_handler::ID;
pub const SOL: u64 = 1_000_000_000;

fn program_so_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/deploy/sra_handler.so")
}

pub fn setup_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    let so_path = program_so_path();
    let so = fs::read(&so_path).unwrap_or_else(|err| {
        panic!(
            "failed to read program binary at {}: {err}. Run `anchor build` first.",
            so_path.display()
        )
    });
    svm.add_program(PROGRAM_ID, so.as_slice()).unwrap();
    svm
}

pub fn funded_keypair(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), lamports).unwrap();
    kp
}

pub fn sample_config() -> SraInitConfig {
    SraInitConfig {
        target_chain_id: 42161,
        target_handler: [0x11u8; 32],
        target_adapter: [0x22u8; 32],
        recipient: [0x33u8; 32],
        app_id: [0x44u8; 32],
    }
}

pub fn sample_target_token() -> [u8; 32] {
    [0xAB; 32]
}

pub fn derive_sra_pda(owner: &Pubkey, config: &SraInitConfig, salt: &[u8; 32]) -> (Pubkey, u8) {
    let config_hash = config.hash(owner);
    Pubkey::find_program_address(&[SEED_SRA, owner.as_ref(), &config_hash, salt], &PROGRAM_ID)
}

pub fn derive_address_book_pda(
    sra_config: &Pubkey,
    target_chain_id: u64,
    target_token: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_ADDRESS_BOOK,
            sra_config.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token,
        ],
        &PROGRAM_ID,
    )
}

pub fn derive_mint_nonce_pda(sra_config: &Pubkey, source_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_MINT_NONCE, sra_config.as_ref(), source_mint.as_ref()],
        &PROGRAM_ID,
    )
}

pub fn derive_adapter_authority_pda(sra_config: &Pubkey, adapter_program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_ADAPTER_AUTHORITY, sra_config.as_ref()],
        adapter_program,
    )
}

pub fn initialize_sra_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    sra_config: &Pubkey,
    config: SraInitConfig,
    salt: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::InitializeSra {
            payer: *payer,
            owner: *owner,
            sra_config: *sra_config,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::InitializeSra { config, salt }.data(),
    }
}

pub fn update_adapters_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    sra_config: &Pubkey,
    adapters: Vec<Pubkey>,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::UpdateAdapters {
            payer: *payer,
            owner: *owner,
            sra_config: *sra_config,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::UpdateAdapters { adapters }.data(),
    }
}

pub fn update_slippage_ix(
    owner: &Pubkey,
    sra_config: &Pubkey,
    allowed_slippage_bps: u16,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::UpdateSlippage {
            owner: *owner,
            sra_config: *sra_config,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::UpdateSlippage {
            allowed_slippage_bps,
        }
        .data(),
    }
}

pub fn set_address_book_entry_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    sra_config: &Pubkey,
    address_book_entry: &Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetAddressBookEntry {
            payer: *payer,
            owner: *owner,
            sra_config: *sra_config,
            address_book_entry: *address_book_entry,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetAddressBookEntry {
            target_chain_id,
            target_token,
            target_decimals,
        }
        .data(),
    }
}

/// Initialize a default SraConfig and return its PDA. Uses fixed salt `[0x11; 32]`.
pub fn init_sra(svm: &mut LiteSVM, payer: &Keypair, owner: &Keypair) -> Pubkey {
    let config = sample_config();
    let salt = [0x11u8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &config, &salt);
    let ix = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &pda, config, salt);
    send_ix(svm, ix, payer, &[owner]).expect("initialize_sra should succeed");
    pda
}

pub fn send_ix(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new(&[ix], Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    let mut all_signers: Vec<&Keypair> = vec![payer];
    all_signers.extend_from_slice(signers);
    tx.sign(&all_signers, blockhash);
    svm.send_transaction(tx).map(|_| ()).map_err(Box::new)
}

pub fn send_ix_expect_err(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
    msg: &str,
) -> TransactionError {
    send_ix(svm, ix, payer, signers).expect_err(msg).err.clone()
}

pub fn fetch_sra_config(svm: &LiteSVM, pda: &Pubkey) -> SraConfig {
    let acct = svm.get_account(pda).expect("sra_config account exists");
    SraConfig::try_deserialize(&mut acct.data.as_slice()).expect("valid SraConfig")
}

pub fn fetch_address_book_entry(svm: &LiteSVM, pda: &Pubkey) -> AddressBookEntry {
    let acct = svm
        .get_account(pda)
        .expect("address_book_entry account exists");
    AddressBookEntry::try_deserialize(&mut acct.data.as_slice()).expect("valid AddressBookEntry")
}

pub fn fetch_mint_nonce(svm: &LiteSVM, pda: &Pubkey) -> MintNonce {
    let acct = svm.get_account(pda).expect("mint_nonce account exists");
    MintNonce::try_deserialize(&mut acct.data.as_slice()).expect("valid MintNonce")
}

// ---------------------------------------------------------------------------
// SPL Token helpers
// ---------------------------------------------------------------------------

pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(owner, mint)
}

pub fn create_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint_authority: &Pubkey,
    decimals: u8,
) -> Pubkey {
    let mint = Keypair::new();
    let rent = svm
        .minimum_balance_for_rent_exemption(SplMint::LEN)
        .max(2_000_000);

    let create_acct_ix = system_instruction::create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        rent,
        SplMint::LEN as u64,
        &TOKEN_PROGRAM_ID,
    );
    let init_mint_ix = spl_token::instruction::initialize_mint(
        &TOKEN_PROGRAM_ID,
        &mint.pubkey(),
        mint_authority,
        None,
        decimals,
    )
    .expect("initialize_mint instruction");

    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new(&[create_acct_ix, init_mint_ix], Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[payer, &mint], blockhash);
    svm.send_transaction(tx)
        .expect("create_mint should succeed");
    mint.pubkey()
}

pub fn create_ata(svm: &mut LiteSVM, payer: &Keypair, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let ata = derive_ata(owner, mint);
    let ix = spl_associated_token_account::instruction::create_associated_token_account(
        &payer.pubkey(),
        owner,
        mint,
        &TOKEN_PROGRAM_ID,
    );
    send_ix(svm, ix, payer, &[]).expect("create_associated_token_account should succeed");
    ata
}

pub fn mint_to(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint_authority: &Keypair,
    mint: &Pubkey,
    to: &Pubkey,
    amount: u64,
) {
    let ix = spl_token::instruction::mint_to(
        &TOKEN_PROGRAM_ID,
        mint,
        to,
        &mint_authority.pubkey(),
        &[],
        amount,
    )
    .expect("mint_to instruction");
    send_ix(svm, ix, payer, &[mint_authority]).expect("mint_to should succeed");
}

pub fn fetch_token_balance(svm: &LiteSVM, ata: &Pubkey) -> u64 {
    let acct = svm.get_account(ata).expect("token account exists");
    let parsed = spl_token::state::Account::unpack(&acct.data).expect("valid SPL TokenAccount");
    parsed.amount
}

/// Creates (or overwrites) an `AddressBookEntry` owned by `owner`. `decimals == 0`
/// leaves the entry tombstoned — useful for the disabled-address-book tests.
pub fn setup_address_book_entry(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    sra_config: &Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Pubkey {
    let (pda, _) = derive_address_book_pda(sra_config, target_chain_id, &target_token);
    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        sra_config,
        &pda,
        target_chain_id,
        target_token,
        target_decimals,
    );
    send_ix(svm, ix, payer, &[owner]).expect("set_address_book_entry should succeed");
    pda
}

// ---------------------------------------------------------------------------
// validate_and_prepare helpers
// ---------------------------------------------------------------------------

pub struct ValidatePrepareAccounts {
    pub sra_config: Pubkey,
    pub mint_nonce: Pubkey,
    pub source_mint: Pubkey,
    pub sra_vault: Pubkey,
    pub adapter_program: Pubkey,
    pub adapter_authority: Pubkey,
    pub adapter_custody: Pubkey,
}

impl ValidatePrepareAccounts {
    pub fn derive(sra_config: &Pubkey, source_mint: &Pubkey, adapter_program: &Pubkey) -> Self {
        let (mint_nonce, _) = derive_mint_nonce_pda(sra_config, source_mint);
        let (adapter_authority, _) = derive_adapter_authority_pda(sra_config, adapter_program);
        Self {
            sra_config: *sra_config,
            mint_nonce,
            source_mint: *source_mint,
            sra_vault: derive_ata(sra_config, source_mint),
            adapter_program: *adapter_program,
            adapter_authority,
            adapter_custody: derive_ata(&adapter_authority, source_mint),
        }
    }
}

pub fn validate_and_prepare_ix(
    fee_payer: &Pubkey,
    owner: &Pubkey,
    accounts: &ValidatePrepareAccounts,
    owner_token_account: &Pubkey,
    address_book_entry: &Pubkey,
    params: PrepareParams,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ValidateAndPrepare {
            fee_payer: *fee_payer,
            owner: *owner,
            sra_config: accounts.sra_config,
            mint_nonce: accounts.mint_nonce,
            source_mint: accounts.source_mint,
            owner_token_account: *owner_token_account,
            sra_vault: accounts.sra_vault,
            adapter_authority: accounts.adapter_authority,
            adapter_custody: accounts.adapter_custody,
            adapter_program: accounts.adapter_program,
            address_book_entry: *address_book_entry,
            token_program: TOKEN_PROGRAM_ID,
            associated_token_program: ATA_PROGRAM_ID,
            system_program: system_program::ID,
            instructions_sysvar: anchor_lang::solana_program::sysvar::instructions::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ValidateAndPrepare { params }.data(),
    }
}

pub const OWNER_STARTING_BALANCE: u64 = 10_000_000;

pub struct ValidatePrepareFixture {
    pub payer: Keypair,
    pub owner: Keypair,
    pub config: SraInitConfig,
    pub sra_config: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub adapter_program: Pubkey,
    pub accounts: ValidatePrepareAccounts,
    pub owner_ata: Pubkey,
}

impl ValidatePrepareFixture {
    pub fn new(svm: &mut LiteSVM) -> Self {
        Self::new_with(svm, 50)
    }

    pub fn new_with(svm: &mut LiteSVM, allowed_slippage_bps: u16) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);

        let config = sample_config();
        let salt = [0xABu8; 32];
        let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &config, &salt);
        let ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            config.clone(),
            salt,
        );
        send_ix(svm, ix, &payer, &[&owner]).expect("initialize_sra");

        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        let target_chain_id = config.target_chain_id;
        let target_token = sample_target_token();
        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &sra_config,
            target_chain_id,
            target_token,
            6,
        );

        // `validate_and_prepare` now requires the adapter to be a loader-owned
        // executable program account, so reuse the loaded handler program as a
        // stand-in.
        let adapter_program = PROGRAM_ID;
        let ix = update_adapters_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            vec![adapter_program],
        );
        send_ix(svm, ix, &payer, &[&owner]).expect("update_adapters");

        let ix = update_slippage_ix(&owner.pubkey(), &sra_config, allowed_slippage_bps);
        send_ix(svm, ix, &payer, &[&owner]).expect("update_slippage");

        let accounts = ValidatePrepareAccounts::derive(&sra_config, &source_mint, &adapter_program);

        let owner_ata = create_ata(svm, &payer, &owner.pubkey(), &source_mint);
        mint_to(
            svm,
            &payer,
            &payer,
            &source_mint,
            &owner_ata,
            OWNER_STARTING_BALANCE,
        );

        Self {
            payer,
            owner,
            config,
            sra_config,
            source_mint,
            target_chain_id,
            target_token,
            adapter_program,
            accounts,
            owner_ata,
        }
    }

    pub fn address_book_entry(&self, target_token: &[u8; 32]) -> Pubkey {
        derive_address_book_pda(&self.sra_config, self.target_chain_id, target_token).0
    }

    pub fn invoke(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        let address_book_entry = self.address_book_entry(&params.target_token);
        let ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &address_book_entry,
            params,
        );
        svm.expire_blockhash();
        let blockhash = svm.latest_blockhash();
        let msg = Message::new(&[ix], Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(msg);
        tx.sign(&[&self.payer, &self.owner], blockhash);
        svm.send_transaction(tx).map_err(Box::new)
    }
}

// ---------------------------------------------------------------------------
// Anchor event extraction
// ---------------------------------------------------------------------------

/// Scan program logs for a `Program data: <base64>` line whose discriminator
/// matches `DepositPrepared`, and borsh-deserialize the remainder.
pub fn extract_deposit_prepared(logs: &[String]) -> Option<sra_handler::events::DepositPrepared> {
    use anchor_lang::{AnchorDeserialize, Discriminator};
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let expected_disc: &[u8] =
        <sra_handler::events::DepositPrepared as Discriminator>::DISCRIMINATOR.as_ref();

    for log in logs {
        let Some(b64) = log.strip_prefix("Program data: ") else {
            continue;
        };
        let Ok(raw) = engine.decode(b64) else {
            continue;
        };
        if raw.len() < expected_disc.len() || &raw[..expected_disc.len()] != expected_disc {
            continue;
        }
        if let Ok(ev) =
            sra_handler::events::DepositPrepared::try_from_slice(&raw[expected_disc.len()..])
        {
            return Some(ev);
        }
    }
    None
}
