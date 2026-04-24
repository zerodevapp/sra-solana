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
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_MINT_ROUTE, SEED_SRA},
    types::{AddressBookEntry, MintNonce, SraConfig, SraMintRoute},
};
use sra_handler::instructions::source::{
    add_mint_route::AddMintRouteParams, initialize_sra::SraInitConfig,
    update_mint_route::UpdateMintRouteParams, validate_and_prepare::PrepareParams,
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

/// `sample_config`-shaped SraInitConfig whose `target_handler` /
/// `target_adapter` are valid EVM-padded bytes32 values (upper 12 bytes zero).
/// Required by `validate_and_prepare`, which converts them to 20-byte EVM
/// addresses when building the canonical ABI message.
pub fn evm_destination_config() -> SraInitConfig {
    let mut target_handler = [0u8; 32];
    target_handler[12..].copy_from_slice(&[0x11u8; 20]);
    let mut target_adapter = [0u8; 32];
    target_adapter[12..].copy_from_slice(&[0x22u8; 20]);
    SraInitConfig {
        target_chain_id: 42161,
        target_handler,
        target_adapter,
        recipient: [0x33u8; 32],
        app_id: [0x44u8; 32],
    }
}

/// EVM-padded `target_token` (20 bytes of 0xAB, upper 12 bytes zero).
pub fn evm_target_token() -> [u8; 32] {
    let mut t = [0u8; 32];
    t[12..].copy_from_slice(&[0xABu8; 20]);
    t
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

pub fn fetch_mint_route(svm: &LiteSVM, pda: &Pubkey) -> SraMintRoute {
    let acct = svm.get_account(pda).expect("mint_route account exists");
    SraMintRoute::try_deserialize(&mut acct.data.as_slice()).expect("valid SraMintRoute")
}

pub fn fetch_mint_nonce(svm: &LiteSVM, pda: &Pubkey) -> MintNonce {
    let acct = svm.get_account(pda).expect("mint_nonce account exists");
    MintNonce::try_deserialize(&mut acct.data.as_slice()).expect("valid MintNonce")
}

pub fn mint_route_exists(svm: &LiteSVM, pda: &Pubkey) -> bool {
    svm.get_account(pda)
        .map(|a| !a.data.is_empty())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// PDA + ATA helpers
// ---------------------------------------------------------------------------

pub fn derive_mint_route_pda(sra_config: &Pubkey, source_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_MINT_ROUTE, sra_config.as_ref(), source_mint.as_ref()],
        &PROGRAM_ID,
    )
}

pub fn derive_mint_nonce_pda(sra_config: &Pubkey, source_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_MINT_NONCE, sra_config.as_ref(), source_mint.as_ref()],
        &PROGRAM_ID,
    )
}

pub fn derive_adapter_authority_pda(
    sra_config: &Pubkey,
    adapter_program: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_ADAPTER_AUTHORITY, sra_config.as_ref()],
        adapter_program,
    )
}

pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(owner, mint)
}

// ---------------------------------------------------------------------------
// SPL Token helpers
// ---------------------------------------------------------------------------

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
    svm.send_transaction(tx).expect("create_mint should succeed");
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
    let parsed =
        spl_token::state::Account::unpack(&acct.data).expect("valid SPL TokenAccount");
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
// Mint-route instruction builders
// ---------------------------------------------------------------------------

pub struct MintRouteAccounts {
    pub sra_config: Pubkey,
    pub mint_route: Pubkey,
    pub mint_nonce: Pubkey,
    pub source_mint: Pubkey,
    pub sra_vault: Pubkey,
    pub address_book_entry: Pubkey,
    pub adapter_program: Pubkey,
    pub adapter_authority: Pubkey,
    pub adapter_custody: Pubkey,
}

impl MintRouteAccounts {
    pub fn derive(
        sra_config: &Pubkey,
        source_mint: &Pubkey,
        target_chain_id: u64,
        target_token: &[u8; 32],
        adapter_program: &Pubkey,
    ) -> Self {
        let (mint_route, _) = derive_mint_route_pda(sra_config, source_mint);
        let (mint_nonce, _) = derive_mint_nonce_pda(sra_config, source_mint);
        let (address_book_entry, _) =
            derive_address_book_pda(sra_config, target_chain_id, target_token);
        let (adapter_authority, _) = derive_adapter_authority_pda(sra_config, adapter_program);
        Self {
            sra_config: *sra_config,
            mint_route,
            mint_nonce,
            source_mint: *source_mint,
            sra_vault: derive_ata(sra_config, source_mint),
            address_book_entry,
            adapter_program: *adapter_program,
            adapter_authority,
            adapter_custody: derive_ata(&adapter_authority, source_mint),
        }
    }
}

pub fn add_mint_route_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    accounts: &MintRouteAccounts,
    params: AddMintRouteParams,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::AddMintRoute {
            payer: *payer,
            owner: *owner,
            sra_config: accounts.sra_config,
            mint_route: accounts.mint_route,
            mint_nonce: accounts.mint_nonce,
            source_mint: accounts.source_mint,
            sra_vault: accounts.sra_vault,
            address_book_entry: accounts.address_book_entry,
            adapter_authority: accounts.adapter_authority,
            adapter_custody: accounts.adapter_custody,
            adapter_program: accounts.adapter_program,
            token_program: TOKEN_PROGRAM_ID,
            associated_token_program: ATA_PROGRAM_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::AddMintRoute { params }.data(),
    }
}

pub fn update_mint_route_ix(
    owner: &Pubkey,
    sra_config: &Pubkey,
    mint_route: &Pubkey,
    address_book_entry: Option<&Pubkey>,
    params: UpdateMintRouteParams,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::UpdateMintRoute {
            owner: *owner,
            sra_config: *sra_config,
            mint_route: *mint_route,
            address_book_entry: address_book_entry.copied(),
        }
        .to_account_metas(None),
        data: sra_handler::instruction::UpdateMintRoute { params }.data(),
    }
}

pub fn close_mint_route_ix(
    owner: &Pubkey,
    sra_config: &Pubkey,
    mint_route: &Pubkey,
    sra_vault: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::CloseMintRoute {
            owner: *owner,
            sra_config: *sra_config,
            mint_route: *mint_route,
            sra_vault: *sra_vault,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::CloseMintRoute {}.data(),
    }
}

/// Bundled setup for mint-route tests: enables an address book entry,
/// creates the source mint, returns everything downstream tests need.
pub struct MintRouteFixture {
    pub payer: Keypair,
    pub owner: Keypair,
    pub sra_config: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub adapter_program: Pubkey,
    pub accounts: MintRouteAccounts,
}

impl MintRouteFixture {
    pub fn new(svm: &mut LiteSVM) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);
        let sra_config = init_sra(svm, &payer, &owner);
        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        let target_chain_id = sample_config().target_chain_id;
        let target_token = [0xAB; 32];
        // Enable address book entry before any add_mint_route call.
        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &sra_config,
            target_chain_id,
            target_token,
            6,
        );
        let adapter_program = Pubkey::new_unique();
        let accounts = MintRouteAccounts::derive(
            &sra_config,
            &source_mint,
            target_chain_id,
            &target_token,
            &adapter_program,
        );
        Self {
            payer,
            owner,
            sra_config,
            source_mint,
            target_chain_id,
            target_token,
            adapter_program,
            accounts,
        }
    }

    pub fn default_params(&self) -> AddMintRouteParams {
        AddMintRouteParams {
            action_idxs: vec![0, 1],
            target_token: self.target_token,
            allowed_slippage_bps: 100,
        }
    }

    pub fn add_default_route(&self, svm: &mut LiteSVM) {
        let ix = add_mint_route_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            self.default_params(),
        );
        send_ix(svm, ix, &self.payer, &[&self.owner])
            .expect("add_mint_route default setup should succeed");
    }
}

// ---------------------------------------------------------------------------
// validate_and_prepare helpers
// ---------------------------------------------------------------------------

pub fn validate_and_prepare_ix(
    fee_payer: &Pubkey,
    depositor: &Pubkey,
    accounts: &MintRouteAccounts,
    depositor_token_account: &Pubkey,
    params: PrepareParams,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ValidateAndPrepare {
            fee_payer: *fee_payer,
            depositor: *depositor,
            sra_config: accounts.sra_config,
            mint_route: accounts.mint_route,
            mint_nonce: accounts.mint_nonce,
            source_mint: accounts.source_mint,
            depositor_token_account: *depositor_token_account,
            sra_vault: accounts.sra_vault,
            adapter_authority: accounts.adapter_authority,
            adapter_custody: accounts.adapter_custody,
            adapter_program: accounts.adapter_program,
            address_book_entry: accounts.address_book_entry,
            token_program: TOKEN_PROGRAM_ID,
            system_program: system_program::ID,
            instructions_sysvar: anchor_lang::solana_program::sysvar::instructions::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ValidateAndPrepare { params }.data(),
    }
}

/// Fully-wired deposit fixture: SRA initialized with an EVM-padded target
/// config, adapter registered in `allowed_adapters`, route created, depositor
/// holds `DEPOSITOR_STARTING_BALANCE` tokens.
pub const DEPOSITOR_STARTING_BALANCE: u64 = 10_000_000;

pub struct ValidatePrepareFixture {
    pub payer: Keypair,
    pub owner: Keypair,
    pub depositor: Keypair,
    pub config: SraInitConfig,
    pub salt: [u8; 32],
    pub sra_config: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub adapter_program: Pubkey,
    pub accounts: MintRouteAccounts,
    pub depositor_ata: Pubkey,
    pub route_params: AddMintRouteParams,
}

impl ValidatePrepareFixture {
    pub fn new(svm: &mut LiteSVM) -> Self {
        Self::new_with(svm, 50, vec![3, 7])
    }

    pub fn new_with(
        svm: &mut LiteSVM,
        allowed_slippage_bps: u16,
        action_idxs: Vec<u64>,
    ) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);
        let depositor = funded_keypair(svm, SOL);

        let config = evm_destination_config();
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
        let target_token = evm_target_token();
        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &sra_config,
            target_chain_id,
            target_token,
            6,
        );

        let adapter_program = Pubkey::new_unique();
        let ix = update_adapters_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            vec![adapter_program],
        );
        send_ix(svm, ix, &payer, &[&owner]).expect("update_adapters");

        let accounts = MintRouteAccounts::derive(
            &sra_config,
            &source_mint,
            target_chain_id,
            &target_token,
            &adapter_program,
        );
        let route_params = AddMintRouteParams {
            action_idxs,
            target_token,
            allowed_slippage_bps,
        };
        let ix = add_mint_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &accounts,
            route_params.clone(),
        );
        send_ix(svm, ix, &payer, &[&owner]).expect("add_mint_route");

        let depositor_ata = create_ata(svm, &payer, &depositor.pubkey(), &source_mint);
        mint_to(
            svm,
            &payer,
            &payer,
            &source_mint,
            &depositor_ata,
            DEPOSITOR_STARTING_BALANCE,
        );

        Self {
            payer,
            owner,
            depositor,
            config,
            salt,
            sra_config,
            source_mint,
            target_chain_id,
            target_token,
            adapter_program,
            accounts,
            depositor_ata,
            route_params,
        }
    }

    pub fn invoke(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
    ) -> Result<
        litesvm::types::TransactionMetadata,
        Box<litesvm::types::FailedTransactionMetadata>,
    > {
        let ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.depositor.pubkey(),
            &self.accounts,
            &self.depositor_ata,
            params,
        );
        svm.expire_blockhash();
        let blockhash = svm.latest_blockhash();
        let msg = Message::new(&[ix], Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(msg);
        tx.sign(&[&self.payer, &self.depositor], blockhash);
        svm.send_transaction(tx).map_err(Box::new)
    }
}

// ---------------------------------------------------------------------------
// Anchor event extraction
// ---------------------------------------------------------------------------

/// Scan program logs for a `Program data: <base64>` line whose discriminator
/// matches `DepositPrepared`, and borsh-deserialize the remainder.
pub fn extract_deposit_prepared(
    logs: &[String],
) -> Option<sra_handler::events::DepositPrepared> {
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
