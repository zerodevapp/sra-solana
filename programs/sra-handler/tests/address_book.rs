mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};

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
    );

    send_ix(&mut svm, ix, &payer, &[&admin]).expect("register should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert_eq!(stored.adapter, adapter);
    assert_eq!(stored.mint, mint);
    assert_eq!(stored.target_chain_id, target_chain_id);
    assert_eq!(stored.target_token, target_token);
    assert!(stored.active);
    assert_eq!(stored.bump, bump);
}

#[test]
fn deactivate_address_book_flips_active() {
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
    );
    send_ix(&mut svm, register_ix, &payer, &[&admin]).expect("register should succeed");

    let deactivate_ix = deactivate_address_book_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        &fee_config,
        &address_book_entry,
        adapter,
        mint,
        target_chain_id,
        target_token,
    );
    send_ix(&mut svm, deactivate_ix, &payer, &[&admin]).expect("deactivate should succeed");

    let stored = fetch_address_book_entry(&svm, &address_book_entry);
    assert!(!stored.active);
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
    );
    send_ix(&mut svm, ix.clone(), &payer, &[&admin]).expect("first register should succeed");

    let result = send_ix(&mut svm, ix, &payer, &[&admin]);
    assert!(result.is_err(), "duplicate register must fail");
}
