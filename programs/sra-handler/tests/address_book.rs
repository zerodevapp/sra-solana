mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use sra_common::errors::SraError;

const SAMPLE_DECIMALS: u8 = 6;
const UPDATED_DECIMALS: u8 = 18;

fn sample_source_mint() -> Pubkey {
    Pubkey::new_from_array([0xCD; 32])
}

fn sample_address_book_args() -> (u64, [u8; 32]) {
    (42_161, [0xAB; 32])
}

#[test]
fn initialize_address_book_root_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = Keypair::new();

    let ix = initialize_address_book_root_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root.pubkey(),
    );
    send_ix(&mut svm, ix, &payer, &[&owner, &address_book_root])
        .expect("initialize_address_book_root should succeed");

    let stored = fetch_address_book_root(&svm, &address_book_root.pubkey());
    assert_eq!(stored.owner, owner.pubkey());
}

#[test]
fn initialize_address_book_root_rejects_owner_as_root() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let owner_before = svm.get_account(&owner.pubkey());

    let ix = initialize_address_book_root_ix(&payer.pubkey(), &owner.pubkey(), &owner.pubkey());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "owner wallet must not become the address book root",
    );

    assert_eq!(err, custom_error(SraError::InvalidAddressBook));
    assert_eq!(svm.get_account(&owner.pubkey()), owner_before);
}

#[test]
fn set_address_book_entry_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, bump) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &target_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );

    send_ix(&mut svm, ix, &payer, &[&owner]).expect("set should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.address_book_root, address_book_root);
    assert_eq!(stored.source_mint, source_mint);
    assert_eq!(stored.target_chain_id, target_chain_id);
    assert_eq!(stored.target_token, target_token);
    assert_eq!(stored.target_decimals, SAMPLE_DECIMALS);
    assert_eq!(stored.bump, bump);
}

#[test]
fn set_address_book_entry_emits_address_book_entry_set() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &target_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&owner])
        .expect("set_address_book_entry should succeed");

    let ev = extract_anchor_event::<sra_handler::events::AddressBookEntrySet>(&meta.logs)
        .expect("AddressBookEntrySet event must be emitted");
    assert_eq!(ev.address_book_root, address_book_root);
    assert_eq!(ev.source_mint, source_mint);
    assert_eq!(ev.target_chain_id, target_chain_id);
    assert_eq!(ev.target_token, target_token);
    assert_eq!(ev.target_decimals, SAMPLE_DECIMALS);
}

#[test]
fn set_address_book_entry_updates_existing_decimals() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &target_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("first set should succeed");

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        UPDATED_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("second set should update");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.target_decimals, UPDATED_DECIMALS);
}

#[test]
fn set_address_book_entry_same_token_allowed_for_different_roots() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner_a = funded_keypair(&mut svm, SOL);
    let owner_b = funded_keypair(&mut svm, SOL);
    let root_a = init_address_book_root(&mut svm, &payer, &owner_a);
    let root_b = init_address_book_root(&mut svm, &payer, &owner_b);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry_a, _) =
        derive_address_book_pda(&root_a, &source_mint, target_chain_id, &target_token);
    let (address_book_entry_b, _) =
        derive_address_book_pda(&root_b, &source_mint, target_chain_id, &target_token);

    let ix_a = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_a.pubkey(),
        &root_a,
        &address_book_entry_a,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix_a, &payer, &[&owner_a]).expect("first root set should succeed");

    let ix_b = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_b.pubkey(),
        &root_b,
        &address_book_entry_b,
        source_mint,
        target_chain_id,
        target_token,
        UPDATED_DECIMALS,
    );
    send_ix(&mut svm, ix_b, &payer, &[&owner_b]).expect("second root set should succeed");

    assert_eq!(
        fetch_address_book_entry(&svm, &address_book_entry_a).target_decimals,
        SAMPLE_DECIMALS
    );
    assert_eq!(
        fetch_address_book_entry(&svm, &address_book_entry_b).target_decimals,
        UPDATED_DECIMALS
    );
}

#[test]
fn set_address_book_entry_same_target_allowed_for_different_source_mints() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint_a = Pubkey::new_from_array([0xAA; 32]);
    let source_mint_b = Pubkey::new_from_array([0xBB; 32]);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (entry_a, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint_a,
        target_chain_id,
        &target_token,
    );
    let (entry_b, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint_b,
        target_chain_id,
        &target_token,
    );
    assert_ne!(
        entry_a, entry_b,
        "same (root, chain, target_token) with different source_mint must derive distinct PDAs",
    );

    let ix_a = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &entry_a,
        source_mint_a,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix_a, &payer, &[&owner]).expect("entry A should succeed");

    let ix_b = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &entry_b,
        source_mint_b,
        target_chain_id,
        target_token,
        UPDATED_DECIMALS,
    );
    send_ix(&mut svm, ix_b, &payer, &[&owner]).expect("entry B should succeed");

    let stored_a = fetch_address_book_entry(&svm, &entry_a);
    let stored_b = fetch_address_book_entry(&svm, &entry_b);
    assert_eq!(stored_a.source_mint, source_mint_a);
    assert_eq!(stored_b.source_mint, source_mint_b);
    assert_eq!(stored_a.target_decimals, SAMPLE_DECIMALS);
    assert_eq!(stored_b.target_decimals, UPDATED_DECIMALS);
}

#[test]
fn set_address_book_entry_zero_target_token_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, _) = sample_address_book_args();
    let zero_token = [0u8; 32];
    let (address_book_entry, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &zero_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        zero_token,
        SAMPLE_DECIMALS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "zero target_token must be rejected",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidTokenInfo.into()),
        )
    );
}

#[test]
fn set_address_book_entry_zero_decimals_tombstones_entry() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &target_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initial set should succeed");

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        0,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("tombstone write should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.target_decimals, 0);
}

#[test]
fn set_address_book_entry_non_owner_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) = derive_address_book_pda(
        &address_book_root,
        &source_mint,
        target_chain_id,
        &target_token,
    );

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        &address_book_root,
        &address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-owner must not set an address book entry",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32),
        )
    );
}

#[test]
fn set_address_book_entry_wrong_pda_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner_a = funded_keypair(&mut svm, SOL);
    let owner_b = funded_keypair(&mut svm, SOL);
    let root_a = init_address_book_root(&mut svm, &payer, &owner_a);
    let root_b = init_address_book_root(&mut svm, &payer, &owner_b);
    let source_mint = sample_source_mint();
    let (target_chain_id, target_token) = sample_address_book_args();
    let (wrong_address_book_entry, _) =
        derive_address_book_pda(&root_b, &source_mint, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_a.pubkey(),
        &root_a,
        &wrong_address_book_entry,
        source_mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner_a],
        "wrong pda must not satisfy address book seeds",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintSeeds as u32),
        )
    );
}
