mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};

const STRANDED_AMOUNT: u64 = 500_000;

/// Owner can recover a stranded Across refund from `adapter_custody`.
#[test]
fn recover_stuck_custody_returns_funds_to_owner() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Simulate a stranded refund: create the custody ATA and mint into it.
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        STRANDED_AMOUNT,
    );

    let before = fetch_token_balance(&svm, &fx.owner_ata);
    let ix = recover_stuck_custody_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        &fx.owner_ata,
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("recover should succeed");

    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        0,
        "custody fully drained",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        before + STRANDED_AMOUNT,
        "owner ATA credited the recovered amount",
    );
}

/// Recovering an empty custody account is rejected with NothingToRecover.
#[test]
fn recover_stuck_custody_nothing_to_recover_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Create the custody ATA but leave it empty (no mint_to).
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );

    let ix = recover_stuck_custody_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        &fx.owner_ata,
    );
    let err = send_ix(&mut svm, ix, &fx.payer, &[&fx.owner])
        .expect_err("empty custody must reject")
        .err
        .clone();

    let expected_code: u32 = sra_across_adapter::AcrossAdapterError::NothingToRecover.into();
    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::Custom(expected_code)),
    );
}

/// Only the SRA owner may recover; a non-owner is rejected by has_one = owner.
#[test]
fn recover_stuck_custody_non_owner_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Fund custody with a stranded refund.
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        STRANDED_AMOUNT,
    );

    // A fresh funded keypair that is NOT the SRA owner, with its own ATA so the
    // ATA constraint is satisfied and `has_one = owner` is what fires.
    let attacker = funded_keypair(&mut svm, SOL);
    let attacker_ata = create_ata(&mut svm, &fx.payer, &attacker.pubkey(), &fx.source_mint);

    let ix = recover_stuck_custody_ix(
        &fx.payer.pubkey(),
        &attacker.pubkey(),
        &fx.sra_config,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        &attacker_ata,
    );
    let err = send_ix(&mut svm, ix, &fx.payer, &[&attacker])
        .expect_err("non-owner recover must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32),
        ),
        "Anchor has_one = owner constraint violation (ConstraintHasOne)",
    );

    // Custody untouched by the failed attempt.
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        STRANDED_AMOUNT,
        "custody balance unchanged after rejected recovery",
    );
}
