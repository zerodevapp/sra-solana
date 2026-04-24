mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::errors::SraError;

const SAMPLE_DECIMALS: u8 = 6;
const UPDATED_DECIMALS: u8 = 18;

fn sample_address_book_args() -> (u64, [u8; 32]) {
    (42161, [0xAB; 32])
}

#[test]
fn set_address_book_entry_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, bump) =
        derive_address_book_pda(&sra_config, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );

    send_ix(&mut svm, ix, &payer, &[&owner]).expect("set should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.sra_config, sra_config);
    assert_eq!(stored.target_chain_id, target_chain_id);
    assert_eq!(stored.target_token, target_token);
    assert_eq!(stored.target_decimals, SAMPLE_DECIMALS);
    assert_eq!(stored.bump, bump);
}

#[test]
fn set_address_book_entry_updates_existing_decimals() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&sra_config, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("first set should succeed");

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
        target_chain_id,
        target_token,
        UPDATED_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("second set should update");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.target_decimals, UPDATED_DECIMALS);
}

#[test]
fn set_address_book_entry_same_token_allowed_for_different_sras() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner_a = funded_keypair(&mut svm, SOL);
    let owner_b = funded_keypair(&mut svm, SOL);
    let sra_config_a = init_sra(&mut svm, &payer, &owner_a);
    let sra_config_b = init_sra(&mut svm, &payer, &owner_b);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry_a, _) =
        derive_address_book_pda(&sra_config_a, target_chain_id, &target_token);
    let (address_book_entry_b, _) =
        derive_address_book_pda(&sra_config_b, target_chain_id, &target_token);

    let ix_a = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_a.pubkey(),
        &sra_config_a,
        &address_book_entry_a,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix_a, &payer, &[&owner_a]).expect("first sra set should succeed");

    let ix_b = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_b.pubkey(),
        &sra_config_b,
        &address_book_entry_b,
        target_chain_id,
        target_token,
        UPDATED_DECIMALS,
    );
    send_ix(&mut svm, ix_b, &payer, &[&owner_b]).expect("second sra set should succeed");

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
fn set_address_book_entry_zero_target_token_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let (target_chain_id, _) = sample_address_book_args();
    let zero_token = [0u8; 32];
    let (address_book_entry, _) =
        derive_address_book_pda(&sra_config, target_chain_id, &zero_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
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
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&sra_config, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initial set should succeed");

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &sra_config,
        &address_book_entry,
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
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&sra_config, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        &sra_config,
        &address_book_entry,
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
    let sra_config_a = init_sra(&mut svm, &payer, &owner_a);
    let sra_config_b = init_sra(&mut svm, &payer, &owner_b);
    let (target_chain_id, target_token) = sample_address_book_args();
    let (wrong_address_book_entry, _) =
        derive_address_book_pda(&sra_config_b, target_chain_id, &target_token);

    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner_a.pubkey(),
        &sra_config_a,
        &wrong_address_book_entry,
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
