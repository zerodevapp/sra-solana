//! Owner-gated custody recovery for both adapters. They expand the same
//! `sra_common::adapter_custody_recovery!`, so the main battery runs against
//! the CCTP adapter (the fixture default) and a mirrored pair proves the
//! Across expansion end-to-end — including each crate's own
//! `NothingToRecover` error code.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction, instruction::InstructionError, pubkey::Pubkey, signer::Signer,
    transaction::TransactionError,
};

const STRANDED_AMOUNT: u64 = 500_000;

/// Simulate a stranded refund: create the custody ATA and mint into it.
fn strand_funds(svm: &mut LiteSVM, fx: &PassiveFixture, amount: u64) {
    create_ata(
        svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );
    if amount > 0 {
        mint_to(
            svm,
            &fx.payer,
            &fx.payer,
            &fx.source_mint,
            &fx.accounts.adapter_custody,
            amount,
        );
    }
}

fn recover_ix(fx: &PassiveFixture, owner: &Pubkey, owner_ata: &Pubkey) -> Instruction {
    recover_stuck_custody_ix(
        &fx.adapter_program,
        &fx.payer.pubkey(),
        owner,
        &fx.sra_config,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
        &fx.accounts.adapter_custody,
        owner_ata,
    )
}

#[test]
fn recover_stuck_custody_returns_funds_to_owner() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    strand_funds(&mut svm, &fx, STRANDED_AMOUNT);

    let before = fetch_token_balance(&svm, &fx.owner_ata);
    let ix = recover_ix(&fx, &fx.owner.pubkey(), &fx.owner_ata);
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

/// Recovering an empty custody account is rejected with the CCTP crate's own
/// NothingToRecover code — covers the macro's per-crate error wiring.
#[test]
fn recover_stuck_custody_nothing_to_recover_rejected() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    strand_funds(&mut svm, &fx, 0);

    let ix = recover_ix(&fx, &fx.owner.pubkey(), &fx.owner_ata);
    let err = send_ix(&mut svm, ix, &fx.payer, &[&fx.owner])
        .expect_err("empty custody must reject")
        .err
        .clone();

    let expected_code: u32 = sra_cctp_adapter::CctpAdapterError::NothingToRecover.into();
    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::Custom(expected_code)),
    );
}

/// Only the SRA owner may recover; a non-owner is rejected by has_one = owner.
#[test]
fn recover_stuck_custody_non_owner_rejected() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    strand_funds(&mut svm, &fx, STRANDED_AMOUNT);

    // A fresh funded keypair that is NOT the SRA owner, with its own ATA so the
    // ATA constraint is satisfied and `has_one = owner` is what fires.
    let attacker = funded_keypair(&mut svm, SOL);
    let attacker_ata = create_ata(&mut svm, &fx.payer, &attacker.pubkey(), &fx.source_mint);

    let ix = recover_ix(&fx, &attacker.pubkey(), &attacker_ata);
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

/// The Across adapter expands the same macro; the happy path proves that
/// expansion is wired end-to-end there too.
#[test]
fn across_recover_stuck_custody_returns_funds_to_owner() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    strand_funds(&mut svm, &fx, STRANDED_AMOUNT);

    let before = fetch_token_balance(&svm, &fx.owner_ata);
    let ix = recover_ix(&fx, &fx.owner.pubkey(), &fx.owner_ata);
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("across recover should succeed");

    assert_eq!(fetch_token_balance(&svm, &fx.accounts.adapter_custody), 0);
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        before + STRANDED_AMOUNT,
    );
}

/// Empty custody rejects with the Across crate's own NothingToRecover code.
#[test]
fn across_recover_stuck_custody_nothing_to_recover_rejected() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    strand_funds(&mut svm, &fx, 0);

    let ix = recover_ix(&fx, &fx.owner.pubkey(), &fx.owner_ata);
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

#[test]
fn adapter_recovery_cannot_access_source_vault_and_only_drains_abnormal_custody() {
    for adapter in [CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID] {
        let mut svm = setup_svm();
        let fx = if adapter == CCTP_ADAPTER_PROGRAM_ID {
            PassiveFixture::new(&mut svm)
        } else {
            PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID])
        };
        set_source_balance(&mut svm, &fx, 2_000_000);
        strand_funds(&mut svm, &fx, STRANDED_AMOUNT);
        let source_before = svm.get_account(&fx.accounts.sra_vault).unwrap();
        let owner_before = svm.get_account(&fx.owner_ata).unwrap();
        let config_before = svm.get_account(&fx.sra_config).unwrap();
        let supply_before = fetch_mint_supply(&svm, &fx.source_mint);
        let ix = recover_stuck_custody_ix(
            &fx.adapter_program,
            &fx.payer.pubkey(),
            &fx.owner.pubkey(),
            &fx.sra_config,
            &fx.accounts.adapter_authority,
            &fx.source_mint,
            &fx.accounts.sra_vault,
            &fx.owner_ata,
        );
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[ix],
            &[&fx.owner],
            None,
            "adapter recovery must reject SRA vault",
        );
        assert_eq!(svm.get_account(&fx.owner_ata).unwrap(), owner_before);
        let metadata = send_ix_with_meta(
            &mut svm,
            recover_ix(&fx, &fx.owner.pubkey(), &fx.owner_ata),
            &fx.payer,
            &[&fx.owner],
        )
        .unwrap();
        if adapter == CCTP_ADAPTER_PROGRAM_ID {
            let event =
                extract_anchor_event::<sra_cctp_adapter::events::CustodyRecovered>(&metadata.logs)
                    .unwrap();
            assert_eq!(event.amount, STRANDED_AMOUNT);
            assert_eq!(event.sra_config, fx.sra_config);
        } else {
            let event = extract_anchor_event::<sra_across_adapter::events::CustodyRecovered>(
                &metadata.logs,
            )
            .unwrap();
            assert_eq!(event.amount, STRANDED_AMOUNT);
            assert_eq!(event.sra_config, fx.sra_config);
        }
        assert_eq!(fetch_token_balance(&svm, &fx.accounts.adapter_custody), 0);
        assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), STRANDED_AMOUNT);
        assert_eq!(
            svm.get_account(&fx.accounts.sra_vault).unwrap(),
            source_before
        );
        assert_eq!(svm.get_account(&fx.sra_config).unwrap(), config_before);
        assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), supply_before);
    }
}
