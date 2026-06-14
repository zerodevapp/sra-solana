mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::signer::Signer;
use solana_sdk::{instruction::InstructionError, transaction::TransactionError};
use sra_common::errors::SraError;

#[test]
fn initialize_sra_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xAAu8; 32];
    let (pda, bump) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data.clone(),
        salt,
    );

    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.owner, owner.pubkey());
    assert_eq!(stored.final_handler, PROGRAM_ID);
    assert_eq!(stored.handler_data_hash, handler_data.hash());
    assert_eq!(stored.salt, salt);
    assert_eq!(stored.bump, bump);
}

#[test]
fn initialize_sra_emits_sra_initialized() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xA5u8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data.clone(),
        salt,
    );
    let meta =
        send_ix_with_meta(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let ev = extract_anchor_event::<sra_handler::events::SraInitialized>(&meta.logs)
        .expect("SraInitialized event must be emitted");
    assert_eq!(ev.sra_config, pda);
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.final_handler, PROGRAM_ID);
    assert_eq!(ev.handler_data_hash, handler_data.hash());
    assert_eq!(ev.salt, salt);
}

#[test]
fn initialize_sra_config_hash_matches_expected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xBBu8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data.clone(),
        salt,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let expected_hash = handler_data.config_hash(&owner.pubkey(), &PROGRAM_ID);
    let (recomputed_pda, _) = solana_sdk::pubkey::Pubkey::find_program_address(
        &[
            sra_common::seeds::SEED_SRA,
            owner.pubkey().as_ref(),
            &expected_hash,
            &salt,
        ],
        &PROGRAM_ID,
    );
    assert_eq!(
        recomputed_pda, pda,
        "on-chain PDA must match off-chain recomputation of config_hash"
    );
}

#[test]
fn initialize_sra_duplicate_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xCCu8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data.clone(),
        salt,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("first initialize_sra should succeed");

    let ix2 = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data,
        salt,
    );
    let result = send_ix(&mut svm, ix2, &payer, &[&owner]);
    assert!(
        result.is_err(),
        "second initialize_sra with same seeds must fail"
    );
}

#[test]
fn initialize_sra_rejects_mismatched_sra_pda() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xDDu8; 32];
    let wrong_salt = [0xEEu8; 32];
    let (wrong_pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &wrong_salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &wrong_pda,
        handler_data,
        salt,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "initialize_sra should reject a PDA derived from different seeds",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintSeeds as u32),
        )
    );
}

#[test]
fn initialize_sra_zero_target_handler_rejected() {
    // Install-path defense. A zero routing field would commit an SRA whose every
    // deposit bridges to address(0) on the target chain (the routing tuple is
    // committed into the SRA's PDA identity at install). `validate()` rejects it
    // before the account is created. The validate() unit suite covers the full
    // field matrix; this is the representative end-to-end install-path case.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let mut handler_data = sample_handler_data(address_book_root);
    handler_data.target_handler = [0u8; 32];
    let salt = [0xF0u8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data,
        salt,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "initialize_sra must reject a zero target_handler route",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidRouteField.into()),
        )
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "no SRA account may be created for a rejected route"
    );
}
