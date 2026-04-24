mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};
use sra_common::errors::SraError;

const SAMPLE_DECIMALS: u8 = 6;

fn sample_address_book_args() -> (Pubkey, Pubkey, u64, [u8; 32]) {
    (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        42161,
        [0xAB; 32],
    )
}

#[test]
fn register_address_book_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, bump) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );

    send_ix(&mut svm, ix, &payer, &[&admin]).expect("register should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.adapter, adapter);
    assert_eq!(stored.mint, mint);
    assert_eq!(stored.target_chain_id, target_chain_id);
    assert_eq!(stored.target_token, target_token);
    assert_eq!(stored.target_decimals, SAMPLE_DECIMALS);
    assert!(stored.active);
    assert_eq!(stored.bump, bump);
}

#[test]
fn register_zero_target_token_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, _) = sample_address_book_args();
    let zero_token = [0u8; 32];
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &zero_token);

    let ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        zero_token,
        SAMPLE_DECIMALS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
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
fn register_zero_decimals_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        0,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "zero target_decimals must be rejected",
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
fn set_active_flips_active_flag() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let register_ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, register_ix, &payer, &[&admin]).expect("register should succeed");

    let deactivate_ix = set_address_book_active_ix(
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        false,
    );
    send_ix(&mut svm, deactivate_ix, &payer, &[&admin]).expect("deactivate should succeed");
    assert!(!fetch_address_book_entry(&svm, &address_book_entry).active);

    let reactivate_ix = set_address_book_active_ix(
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        true,
    );
    send_ix(&mut svm, reactivate_ix, &payer, &[&admin]).expect("reactivate should succeed");
    assert!(fetch_address_book_entry(&svm, &address_book_entry).active);
}

#[test]
fn register_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let ix = register_address_book_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not register an address book entry",
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
fn register_duplicate_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, ix.clone(), &payer, &[&admin]).expect("first register should succeed");

    let result = send_ix(&mut svm, ix, &payer, &[&admin]);
    assert!(result.is_err(), "duplicate register must fail");
}

#[test]
fn set_active_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let fee_config = seed_fee_config(&mut svm, &admin.pubkey());
    let (adapter, mint, target_chain_id, target_token) = sample_address_book_args();
    let (address_book_entry, _) =
        derive_address_book_pda(&adapter, &mint, target_chain_id, &target_token);

    let register_ix = register_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        SAMPLE_DECIMALS,
    );
    send_ix(&mut svm, register_ix, &payer, &[&admin]).expect("register should succeed");

    let deactivate_ix = set_address_book_active_ix(
        &attacker.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
        false,
    );
    let err = send_ix_expect_err(
        &mut svm,
        deactivate_ix,
        &payer,
        &[&attacker],
        "non-admin must not change active flag",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32),
        )
    );

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert!(stored.active);
}
