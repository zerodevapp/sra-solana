mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::{constants::MAX_SLIPPAGE_BPS, errors::SraError};

#[test]
fn update_slippage_owner_sets_value() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix = update_slippage_ix(&owner.pubkey(), &pda, 75);
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("update_slippage should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.allowed_slippage_bps, 75);
}

#[test]
fn update_slippage_emits_slippage_updated() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let allowed_slippage_bps = 75;
    let ix = update_slippage_ix(&owner.pubkey(), &pda, allowed_slippage_bps);
    let meta =
        send_ix_with_meta(&mut svm, ix, &payer, &[&owner]).expect("update_slippage should succeed");

    let ev = extract_anchor_event::<sra_handler::events::SlippageUpdated>(&meta.logs)
        .expect("SlippageUpdated event must be emitted");
    assert_eq!(ev.sra_config, pda);
    assert_eq!(ev.allowed_slippage_bps, allowed_slippage_bps);
}

#[test]
fn update_slippage_owner_replaces_value() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix1 = update_slippage_ix(&owner.pubkey(), &pda, 50);
    send_ix(&mut svm, ix1, &payer, &[&owner]).expect("first update should succeed");

    let ix2 = update_slippage_ix(&owner.pubkey(), &pda, 125);
    send_ix(&mut svm, ix2, &payer, &[&owner]).expect("second update should succeed");

    let stored = fetch_sra_config(&svm, &pda);
    assert_eq!(stored.allowed_slippage_bps, 125);
}

#[test]
fn update_slippage_non_owner_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix = update_slippage_ix(&attacker.pubkey(), &pda, 75);
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-owner must not update slippage",
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
fn update_slippage_over_max_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let pda = init_sra(&mut svm, &payer, &owner);

    let ix = update_slippage_ix(&owner.pubkey(), &pda, MAX_SLIPPAGE_BPS + 1);
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "slippage above MAX_SLIPPAGE_BPS must fail",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidSlippage.into()),
        )
    );
}
