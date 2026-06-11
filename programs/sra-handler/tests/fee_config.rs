mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::TransactionError,
};
use sra_common::{constants::MAX_FEE_BPS, errors::SraError};

const PROTOCOL_FEE_BPS: u16 = 30;
const APP_FEE_BPS: u16 = 20;

fn sample_app_id() -> [u8; 32] {
    [0x77u8; 32]
}

/// Initialize the singleton fee config and return the admin keypair that owns it.
fn setup_fee_config(svm: &mut LiteSVM, payer: &Keypair) -> Keypair {
    let admin = funded_keypair(svm, SOL);
    let ix = initialize_fee_config_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        PROTOCOL_FEE_BPS,
        Pubkey::new_unique(),
    );
    send_ix(svm, ix, payer, &[&admin]).expect("initialize_fee_config should succeed");
    admin
}

// --------------------------------------------------------------------------
// initialize_fee_config
// --------------------------------------------------------------------------

#[test]
fn initialize_fee_config_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let recipient = Pubkey::new_unique();
    let (fee_config, bump) = derive_fee_config_pda();

    let ix = initialize_fee_config_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        PROTOCOL_FEE_BPS,
        recipient,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("initialize_fee_config should succeed");

    let stored = fetch_fee_config(&svm, &fee_config);
    assert_eq!(stored.admin, admin.pubkey());
    assert_eq!(stored.protocol_fee_bps, PROTOCOL_FEE_BPS);
    assert_eq!(stored.protocol_fee_recipient, recipient);
    assert_eq!(stored.bump, bump);
}

#[test]
fn initialize_fee_config_duplicate_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let ix2 = initialize_fee_config_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        PROTOCOL_FEE_BPS,
        Pubkey::new_unique(),
    );
    // An `init` collision surfaces as a non-stable system-level error, so assert
    // failure only — matching the initialize_sra_duplicate_rejected convention.
    let result = send_ix(&mut svm, ix2, &payer, &[&admin]);
    assert!(
        result.is_err(),
        "second initialize_fee_config on the singleton PDA must fail"
    );
}

#[test]
fn initialize_fee_config_bps_over_max_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);

    let ix = initialize_fee_config_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        MAX_FEE_BPS + 1,
        Pubkey::new_unique(),
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "protocol_fee_bps over MAX_FEE_BPS must be rejected at write time",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::FeeTooHigh.into())
        ),
    );
}

#[test]
fn initialize_fee_config_emits_event() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let recipient = Pubkey::new_unique();
    let (fee_config, _) = derive_fee_config_pda();

    let ix = initialize_fee_config_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        PROTOCOL_FEE_BPS,
        recipient,
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("initialize_fee_config should succeed");

    let ev = extract_anchor_event::<sra_handler::events::FeeConfigInitialized>(&meta.logs)
        .expect("FeeConfigInitialized event must be emitted");
    assert_eq!(ev.fee_config, fee_config);
    assert_eq!(ev.admin, admin.pubkey());
    assert_eq!(ev.protocol_fee_bps, PROTOCOL_FEE_BPS);
    assert_eq!(ev.protocol_fee_recipient, recipient);
}

// --------------------------------------------------------------------------
// register_app_fee
// --------------------------------------------------------------------------

#[test]
fn register_app_fee_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let app_id = sample_app_id();
    let app_recipient = Pubkey::new_unique();
    let (app_fee_entry, bump) = derive_app_fee_pda(&app_id);

    let ix = register_app_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        app_id,
        APP_FEE_BPS,
        app_recipient,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("register_app_fee should succeed");

    let stored = fetch_app_fee_entry(&svm, &app_fee_entry);
    assert_eq!(stored.app_id, app_id);
    assert_eq!(stored.app_fee_bps, APP_FEE_BPS);
    assert_eq!(stored.app_fee_recipient, app_recipient);
    assert_eq!(stored.bump, bump);
}

#[test]
fn register_app_fee_cap_exceeded_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    // protocol (30) + app must stay <= 1000; this pushes the sum to 1001.
    let over_cap_app_bps = MAX_FEE_BPS - PROTOCOL_FEE_BPS + 1;
    let ix = register_app_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        sample_app_id(),
        over_cap_app_bps,
        Pubkey::new_unique(),
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "protocol_fee_bps + app_fee_bps over MAX_FEE_BPS must be rejected",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::FeeTooHigh.into())
        ),
    );
}

#[test]
fn register_app_fee_cap_boundary_succeeds() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    // protocol (30) + app (970) == 1000 exactly: the cap is inclusive.
    let boundary_app_bps = MAX_FEE_BPS - PROTOCOL_FEE_BPS;
    let app_id = sample_app_id();
    let ix = register_app_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        app_id,
        boundary_app_bps,
        Pubkey::new_unique(),
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("sum == MAX_FEE_BPS must be accepted");

    let (app_fee_entry, _) = derive_app_fee_pda(&app_id);
    assert_eq!(
        fetch_app_fee_entry(&svm, &app_fee_entry).app_fee_bps,
        boundary_app_bps
    );
}

#[test]
fn register_app_fee_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_fee_config(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = register_app_fee_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        sample_app_id(),
        APP_FEE_BPS,
        Pubkey::new_unique(),
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not register an app fee",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32)
        ),
    );
}

#[test]
fn register_app_fee_zero_app_id_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let ix = register_app_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        [0u8; 32],
        APP_FEE_BPS,
        Pubkey::new_unique(),
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "zero app_id (the \"no app fee\" sentinel) must not be registrable",
    );

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAppId.into())
        ),
    );
}

#[test]
fn register_app_fee_update_existing() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let app_id = sample_app_id();
    send_ix(
        &mut svm,
        register_app_fee_ix(
            &payer.pubkey(),
            &admin.pubkey(),
            app_id,
            APP_FEE_BPS,
            Pubkey::new_unique(),
        ),
        &payer,
        &[&admin],
    )
    .expect("first register_app_fee should succeed");

    let updated_recipient = Pubkey::new_unique();
    let updated_bps = APP_FEE_BPS + 5;
    send_ix(
        &mut svm,
        register_app_fee_ix(
            &payer.pubkey(),
            &admin.pubkey(),
            app_id,
            updated_bps,
            updated_recipient,
        ),
        &payer,
        &[&admin],
    )
    .expect("re-registering an existing app_id must update it");

    let (app_fee_entry, _) = derive_app_fee_pda(&app_id);
    let stored = fetch_app_fee_entry(&svm, &app_fee_entry);
    assert_eq!(stored.app_fee_bps, updated_bps);
    assert_eq!(stored.app_fee_recipient, updated_recipient);
}

#[test]
fn register_app_fee_emits_event() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let app_id = sample_app_id();
    let app_recipient = Pubkey::new_unique();
    let (app_fee_entry, _) = derive_app_fee_pda(&app_id);

    let meta = send_ix_with_meta(
        &mut svm,
        register_app_fee_ix(
            &payer.pubkey(),
            &admin.pubkey(),
            app_id,
            APP_FEE_BPS,
            app_recipient,
        ),
        &payer,
        &[&admin],
    )
    .expect("register_app_fee should succeed");

    let ev = extract_anchor_event::<sra_handler::events::AppFeeRegistered>(&meta.logs)
        .expect("AppFeeRegistered event must be emitted");
    assert_eq!(ev.app_fee_entry, app_fee_entry);
    assert_eq!(ev.app_id, app_id);
    assert_eq!(ev.app_fee_bps, APP_FEE_BPS);
    assert_eq!(ev.app_fee_recipient, app_recipient);
}
