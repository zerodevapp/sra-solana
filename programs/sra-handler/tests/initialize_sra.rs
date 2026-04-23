mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::signer::Signer;
use solana_sdk::{instruction::InstructionError, transaction::TransactionError};
use sra_common::seeds::config_hash;

#[test]
fn initialize_sra_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);

    let config = sample_config();
    let salt = [0xAAu8; 32];
    let (pda, bump) = derive_sra_pda(&owner.pubkey(), &config, &salt);

    let ix = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &pda, config.clone(), salt);

    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.owner, owner.pubkey());
    assert_eq!(stored.target_chain_id, config.target_chain_id);
    assert_eq!(stored.target_handler, config.target_handler);
    assert_eq!(stored.target_adapter, config.target_adapter);
    assert_eq!(stored.recipient, config.recipient);
    assert_eq!(stored.app_id, config.app_id);
    assert_eq!(stored.salt, salt);
    assert!(stored.allowed_adapters.is_empty());
    assert_eq!(stored.bump, bump);
}

#[test]
fn initialize_sra_config_hash_matches_expected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);

    let config = sample_config();
    let salt = [0xBBu8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &config, &salt);

    let ix = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &pda, config.clone(), salt);
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("initialize_sra should succeed");

    let expected_hash = config_hash(
        &owner.pubkey(),
        config.target_chain_id,
        &config.target_handler,
        &config.target_adapter,
        &config.recipient,
        &config.app_id,
    );
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

    let config = sample_config();
    let salt = [0xCCu8; 32];
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &config, &salt);

    let ix = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &pda, config.clone(), salt);
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("first initialize_sra should succeed");

    // Second call with same seeds should fail (account already exists).
    let ix2 = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &pda, config, salt);
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

    let config = sample_config();
    let salt = [0xDDu8; 32];
    let wrong_salt = [0xEEu8; 32];
    let (wrong_pda, _) = derive_sra_pda(&owner.pubkey(), &config, &wrong_salt);

    let ix = initialize_sra_ix(&payer.pubkey(), &owner.pubkey(), &wrong_pda, config, salt);
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
