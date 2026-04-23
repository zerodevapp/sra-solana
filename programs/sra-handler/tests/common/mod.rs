// Each integration-test binary (initialize_sra, update_adapters, ...) pulls in this module
// but only uses a subset of its helpers. Silence per-binary dead_code warnings on the shared set.
#![allow(dead_code)]

use anchor_lang::{
    prelude::{AccountSerialize, Space},
    AccountDeserialize, InstructionData, ToAccountMetas,
};
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    hash::hash,
    instruction::AccountMeta,
    instruction::Instruction,
    message::Message,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::program as system_program;
use sra_common::{
    seeds::{SEED_ADDRESS_BOOK, SEED_FEE_CONFIG, SEED_SRA},
    types::{AddressBookEntry, FeeConfig, SraConfig},
};
use sra_handler::instructions::source::initialize_sra::SraInitConfig;
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

pub fn derive_sra_pda(owner: &Pubkey, config: &SraInitConfig, salt: &[u8; 32]) -> (Pubkey, u8) {
    let config_hash = config.hash(owner);
    Pubkey::find_program_address(&[SEED_SRA, owner.as_ref(), &config_hash, salt], &PROGRAM_ID)
}

pub fn derive_fee_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_FEE_CONFIG], &PROGRAM_ID)
}

pub fn derive_address_book_pda(
    adapter: &Pubkey,
    mint: &Pubkey,
    target_chain_id: u64,
    target_token: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_ADDRESS_BOOK,
            adapter.as_ref(),
            mint.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token,
        ],
        &PROGRAM_ID,
    )
}

fn instruction_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{name}");
    let hash = hash(preimage.as_bytes()).to_bytes();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}

fn address_book_instruction_data(
    name: &str,
    adapter: Pubkey,
    mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + 32 + 32 + 8 + 32);
    data.extend_from_slice(&instruction_discriminator(name));
    data.extend_from_slice(adapter.as_ref());
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&target_chain_id.to_le_bytes());
    data.extend_from_slice(&target_token);
    data
}

pub fn seed_fee_config(svm: &mut LiteSVM, admin: &Pubkey) -> Pubkey {
    let (fee_config_pda, bump) = derive_fee_config_pda();
    let fee_config = FeeConfig {
        admin: *admin,
        protocol_fee_bps: 0,
        protocol_fee_recipient: Pubkey::new_unique(),
        bump,
    };

    let mut data = Vec::with_capacity(8 + FeeConfig::INIT_SPACE);
    fee_config
        .try_serialize(&mut data)
        .expect("fee config serializes");

    svm.set_account(
        fee_config_pda,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len()),
            data,
            owner: PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("fee config seeded");

    fee_config_pda
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

pub fn register_address_book_ix(
    payer: &Pubkey,
    admin: &Pubkey,
    fee_config: &Pubkey,
    address_book_entry: &Pubkey,
    adapter: Pubkey,
    mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new_readonly(*fee_config, false),
            AccountMeta::new(*address_book_entry, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: address_book_instruction_data(
            "register_address_book_entry",
            adapter,
            mint,
            target_chain_id,
            target_token,
        ),
    }
}

pub fn deactivate_address_book_ix(
    payer: &Pubkey,
    admin: &Pubkey,
    fee_config: &Pubkey,
    address_book_entry: &Pubkey,
    adapter: Pubkey,
    mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new_readonly(*fee_config, false),
            AccountMeta::new(*address_book_entry, false),
        ],
        data: address_book_instruction_data(
            "deactivate_address_book_entry",
            adapter,
            mint,
            target_chain_id,
            target_token,
        ),
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
    AddressBookEntry::try_deserialize(&mut acct.data.as_slice())
        .expect("valid AddressBookEntry")
}
