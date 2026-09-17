mod common;

use anchor_lang::{error::ErrorCode, InstructionData, Space};
use common::*;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use sra_common::errors::SraError;
use sra_common::{constants::MAX_ADAPTERS, types::SraConfig};

#[test]
fn initialize_sra_zero_initiator_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xAD; 32];
    let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data,
        salt,
        Pubkey::default(),
        source_mint,
    );
    let err = send_ix_expect_err(&mut svm, ix, &payer, &[&owner], "zero initiator must fail");
    assert_eq!(err, custom_error(SraError::InvalidInitiator));
    assert!(svm.get_account(&sra).is_none());
    assert!(svm.get_account(&derive_ata(&sra, &source_mint)).is_none());
}

#[test]
fn initialize_sra_preserves_prefunded_vault() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xAE; 32];
    let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let vault = create_ata(&mut svm, &payer, &sra, &source_mint);
    mint_to(&mut svm, &payer, &payer, &source_mint, &vault, 1_000_000);
    let before = svm.get_account(&vault).unwrap();
    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data,
        salt,
        owner.pubkey(),
        source_mint,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).unwrap();
    assert_eq!(svm.get_account(&vault).unwrap(), before);
    assert_token_account(&svm, &vault, source_mint, sra, 1_000_000);
}

#[test]
fn initialize_sra_requires_owner_signature() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xAF; 32];
    let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let mut ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data,
        salt,
        owner.pubkey(),
        source_mint,
    );
    ix.accounts
        .iter_mut()
        .find(|meta| meta.pubkey == owner.pubkey())
        .unwrap()
        .is_signer = false;
    let err = send_ix_expect_err(&mut svm, ix, &payer, &[], "owner must sign setup");
    assert_eq!(err, anchor_error(ErrorCode::AccountNotSigner));
    assert!(svm.get_account(&sra).is_none());
}

#[test]
fn initialize_sra_rejects_foreign_address_book_owner() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let foreign_owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &foreign_owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xB0; 32];
    let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data,
        salt,
        owner.pubkey(),
        source_mint,
    );
    let err = send_ix_expect_err(&mut svm, ix, &payer, &[&owner], "foreign root must fail");
    assert_eq!(err, custom_error(SraError::InvalidAddressBook));
    assert!(svm.get_account(&sra).is_none());
}

#[test]
fn initialize_sra_rejects_wrong_config_hash() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let handler_data = sample_handler_data(address_book_root);
    let salt = [0xB1; 32];
    let config_hash = [0xB2; 32];
    let (sra, _) = Pubkey::find_program_address(
        &[
            sra_common::seeds::SEED_SRA,
            owner.pubkey().as_ref(),
            &config_hash,
            &salt,
        ],
        &PROGRAM_ID,
    );
    let mut ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data.clone(),
        salt,
        owner.pubkey(),
        source_mint,
    );
    ix.data = sra_handler::instruction::InitializeSra {
        config_hash,
        handler_data,
        salt,
        initiator: owner.pubkey(),
    }
    .data();
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "wrong config hash must fail",
    );
    assert_eq!(err, custom_error(SraError::InvalidHandlerData));
    assert!(svm.get_account(&sra).is_none());
    assert!(svm.get_account(&derive_ata(&sra, &source_mint)).is_none());
}

#[test]
fn initialize_sra_stores_route_initiator_and_vault() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let initiator = Keypair::new();
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let mut handler_data = sample_handler_data(address_book_root);
    handler_data.allowed_adapters = (0..MAX_ADAPTERS).map(|_| Pubkey::new_unique()).collect();
    let salt = [0xAC; 32];
    let (sra, bump) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra,
        handler_data.clone(),
        salt,
        initiator.pubkey(),
        source_mint,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let state = fetch_sra_config(&svm, &sra);
    assert_eq!(state.owner, owner.pubkey());
    assert_eq!(state.final_handler, PROGRAM_ID);
    assert_eq!(state.handler_data, handler_data);
    assert_eq!(state.salt, salt);
    assert_eq!(state.initiator, initiator.pubkey());
    assert!(state.active);
    assert_eq!(state.deposit_nonce, 0);
    assert_eq!(state.bump, bump);
    assert_eq!(
        svm.get_account(&sra).unwrap().data.len(),
        8 + SraConfig::INIT_SPACE
    );
    assert_token_account(&svm, &derive_ata(&sra, &source_mint), source_mint, sra, 0);
}

#[test]
fn initialize_sra_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
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
        owner.pubkey(),
        source_mint,
    );

    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.owner, owner.pubkey());
    assert_eq!(stored.final_handler, PROGRAM_ID);
    assert_eq!(stored.handler_data, handler_data);
    assert_eq!(stored.salt, salt);
    assert_eq!(stored.bump, bump);
}

#[test]
fn initialize_sra_emits_sra_initialized() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
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
        owner.pubkey(),
        source_mint,
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
    assert_eq!(ev.initiator, owner.pubkey());
    assert!(ev.active);
    assert_eq!(ev.deposit_nonce, 0);
    assert_eq!(ev.initial_source_mint, source_mint);
    assert_eq!(ev.initial_source_vault, derive_ata(&pda, &source_mint));
}

#[test]
fn initialize_sra_config_hash_matches_expected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
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
        owner.pubkey(),
        source_mint,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let expected_hash = handler_data.config_hash(&owner.pubkey(), &PROGRAM_ID);
    let (recomputed_pda, _) = solana_pubkey::Pubkey::find_program_address(
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
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
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
        owner.pubkey(),
        source_mint,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("first initialize_sra should succeed");

    let ix2 = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data,
        salt,
        owner.pubkey(),
        source_mint,
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
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
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
        owner.pubkey(),
        source_mint,
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
fn initialize_sra_zero_recipient_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let address_book_root = init_address_book_root(&mut svm, &payer, &owner);

    let mut handler_data = sample_handler_data(address_book_root);
    handler_data.recipient = [0u8; 32];
    let salt = [0xF0u8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);

    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &pda,
        handler_data,
        salt,
        owner.pubkey(),
        source_mint,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "initialize_sra must reject a zero recipient route",
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
