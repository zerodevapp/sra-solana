mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};
use sra_common::{constants::MAX_ADAPTERS, errors::SraError};

#[test]
fn update_adapters_owner_sets_list() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let adapter_a = Pubkey::new_unique();
    let adapter_b = Pubkey::new_unique();

    let ix = update_adapters_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &pda,
        vec![adapter_a, adapter_b],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("update_adapters should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.allowed_adapters, vec![adapter_a, adapter_b]);
}

#[test]
fn update_adapters_owner_replaces_list() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let adapter_a = Pubkey::new_unique();
    let adapter_b = Pubkey::new_unique();
    let adapter_c = Pubkey::new_unique();

    // Set [A, B], then replace with [C].
    let ix1 = update_adapters_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &pda,
        vec![adapter_a, adapter_b],
    );
    send_ix(&mut svm, ix1, &payer, &[&owner]).expect("first set should succeed");

    let ix2 = update_adapters_ix(&payer.pubkey(), &owner.pubkey(), &pda, vec![adapter_c]);
    send_ix(&mut svm, ix2, &payer, &[&owner]).expect("replace should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.allowed_adapters, vec![adapter_c]);
}

#[test]
fn update_adapters_empty_list_clears() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix1 = update_adapters_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &pda,
        vec![Pubkey::new_unique()],
    );
    send_ix(&mut svm, ix1, &payer, &[&owner]).expect("set should succeed");

    let ix2 = update_adapters_ix(&payer.pubkey(), &owner.pubkey(), &pda, vec![]);
    send_ix(&mut svm, ix2, &payer, &[&owner]).expect("clear should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert!(stored.allowed_adapters.is_empty());
}

#[test]
fn update_adapters_non_owner_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix = update_adapters_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        &pda,
        vec![Pubkey::new_unique()],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-owner must not update adapters",
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
fn update_adapters_max_exceeded_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let too_many: Vec<Pubkey> = (0..=MAX_ADAPTERS).map(|_| Pubkey::new_unique()).collect();

    let ix = update_adapters_ix(&payer.pubkey(), &owner.pubkey(), &pda, too_many);
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "list larger than MAX_ADAPTERS must fail",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::TooManyAdapters.into()),
        )
    );
}
