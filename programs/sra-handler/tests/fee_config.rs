mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use sra_common::{constants::MAX_FEE_BPS, errors::SraError};

const DEFAULT_FEE_BPS: u16 = 30;
const TIER_THRESHOLD: u64 = 1_000_000;
const TIER_BELOW_BPS: u16 = 50;
const TIER_ABOVE_BPS: u16 = 10;

/// Initialize the singleton fee config and return the admin keypair that owns it.
fn setup_fee_config(svm: &mut LiteSVM, payer: &Keypair) -> Keypair {
    let admin = funded_keypair(svm, SOL);
    let ix = initialize_fee_config_ix(&payer.pubkey(), &admin.pubkey(), Pubkey::new_unique());
    send_ix(svm, ix, payer, &[&admin]).expect("initialize_fee_config should succeed");
    admin
}

/// Fee config + the global protocol fee entry.
fn setup_with_protocol_fee(svm: &mut LiteSVM, payer: &Keypair) -> Keypair {
    let admin = setup_fee_config(svm, payer);
    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS,
        false,
        false,
    );
    send_ix(svm, ix, payer, &[&admin]).expect("set_protocol_fee should succeed");
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

    let ix = initialize_fee_config_ix(&payer.pubkey(), &admin.pubkey(), recipient);
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("initialize_fee_config should succeed");

    let stored = fetch_fee_config(&svm, &fee_config);
    assert_eq!(stored.admin, admin.pubkey());
    assert_eq!(stored.pending_admin, Pubkey::default());
    assert_eq!(stored.fee_recipient, recipient);
    assert_eq!(stored.bump, bump);

    let ev = extract_anchor_event::<sra_handler::events::FeeConfigInitialized>(&meta.logs)
        .expect("FeeConfigInitialized event must be emitted");
    assert_eq!(ev.fee_config, fee_config);
    assert_eq!(ev.admin, admin.pubkey());
    assert_eq!(ev.fee_recipient, recipient);
}

#[test]
fn initialize_fee_config_duplicate_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    // An `init` collision surfaces as a non-stable system-level error, so assert
    // failure only — matching the initialize_sra_duplicate_rejected convention.
    let ix2 = initialize_fee_config_ix(&payer.pubkey(), &admin.pubkey(), Pubkey::new_unique());
    let result = send_ix(&mut svm, ix2, &payer, &[&admin]);
    assert!(
        result.is_err(),
        "second initialize_fee_config on the singleton PDA must fail"
    );
}

#[test]
fn initialize_fee_config_default_recipient_allowed() {
    // Pubkey::default() recipient == fees disabled; the disabled state must be
    // representable from genesis.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = funded_keypair(&mut svm, SOL);
    let (fee_config, _) = derive_fee_config_pda();

    let ix = initialize_fee_config_ix(&payer.pubkey(), &admin.pubkey(), Pubkey::default());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("default recipient must be accepted");

    assert_eq!(
        fetch_fee_config(&svm, &fee_config).fee_recipient,
        Pubkey::default()
    );
}

// --------------------------------------------------------------------------
// set_fee_recipient
// --------------------------------------------------------------------------

#[test]
fn set_fee_recipient_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let (fee_config, _) = derive_fee_config_pda();
    let old_recipient = fetch_fee_config(&svm, &fee_config).fee_recipient;
    let new_recipient = Pubkey::new_unique();

    let ix = set_fee_recipient_ix(&admin.pubkey(), new_recipient);
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("set_fee_recipient should succeed");

    assert_eq!(
        fetch_fee_config(&svm, &fee_config).fee_recipient,
        new_recipient
    );

    let ev = extract_anchor_event::<sra_handler::events::FeeRecipientUpdated>(&meta.logs)
        .expect("FeeRecipientUpdated event must be emitted");
    assert_eq!(ev.old_fee_recipient, old_recipient);
    assert_eq!(ev.new_fee_recipient, new_recipient);
}

#[test]
fn set_fee_recipient_default_disables() {
    // Setting Pubkey::default() is the global kill switch (EVM address(0)).
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let (fee_config, _) = derive_fee_config_pda();

    let ix = set_fee_recipient_ix(&admin.pubkey(), Pubkey::default());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("default recipient must be settable");

    assert_eq!(
        fetch_fee_config(&svm, &fee_config).fee_recipient,
        Pubkey::default()
    );
}

#[test]
fn set_fee_recipient_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_fee_config(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = set_fee_recipient_ix(&attacker.pubkey(), Pubkey::new_unique());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not set the fee recipient",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

// --------------------------------------------------------------------------
// set_protocol_fee
// --------------------------------------------------------------------------

#[test]
fn set_protocol_fee_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let (protocol_fee_config, bump) = derive_protocol_fee_pda();

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS,
        true,
        true,
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("set_protocol_fee should succeed");

    let stored = fetch_protocol_fee_config(&svm, &protocol_fee_config);
    assert_eq!(stored.default_fee_bps, DEFAULT_FEE_BPS);
    assert!(stored.allow_override);
    assert!(stored.allow_sponsored);
    assert_eq!(stored.bump, bump);

    let ev = extract_anchor_event::<sra_handler::events::ProtocolFeeUpdated>(&meta.logs)
        .expect("ProtocolFeeUpdated event must be emitted");
    assert_eq!(ev.default_fee_bps, DEFAULT_FEE_BPS);
    assert!(ev.allow_override);
    assert!(ev.allow_sponsored);
}

#[test]
fn set_protocol_fee_over_max_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        MAX_FEE_BPS + 1,
        false,
        false,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "default_fee_bps over MAX_FEE_BPS must be rejected",
    );
    assert_eq!(err, custom_error(SraError::FeeTooHigh));
}

#[test]
fn set_protocol_fee_upsert_updates() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let (protocol_fee_config, _) = derive_protocol_fee_pda();

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS + 5,
        true,
        true,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("re-setting an existing app must update");

    let stored = fetch_protocol_fee_config(&svm, &protocol_fee_config);
    assert_eq!(stored.default_fee_bps, DEFAULT_FEE_BPS + 5);
    assert!(stored.allow_override);
    assert!(stored.allow_sponsored);
}

#[test]
fn set_protocol_fee_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_fee_config(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        DEFAULT_FEE_BPS,
        false,
        false,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not set a protocol fee",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

// --------------------------------------------------------------------------
// remove_protocol_fee
// --------------------------------------------------------------------------

#[test]
fn remove_protocol_fee_closes_pda() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let (protocol_fee_config, _) = derive_protocol_fee_pda();
    assert!(svm.get_account(&protocol_fee_config).is_some());

    let ix = remove_protocol_fee_ix(&admin.pubkey());
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("remove_protocol_fee should succeed");

    // PDA existence == isSet: a closed account means "fee not configured".
    let closed = svm.get_account(&protocol_fee_config);
    assert!(
        closed.is_none() || closed.unwrap().lamports == 0,
        "protocol_fee_config PDA must be closed"
    );

    let _ev = extract_anchor_event::<sra_handler::events::ProtocolFeeRemoved>(&meta.logs)
        .expect("ProtocolFeeRemoved event must be emitted");
}

#[test]
fn remove_protocol_fee_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_with_protocol_fee(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = remove_protocol_fee_ix(&attacker.pubkey());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not remove a protocol fee",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

// --------------------------------------------------------------------------
// set_asset_fee_tier / remove_asset_fee_tier
// --------------------------------------------------------------------------

#[test]
fn set_asset_fee_tier_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let mint = Pubkey::new_unique();
    let (asset_fee_tier, bump) = derive_asset_fee_tier_pda(&mint);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("set_asset_fee_tier should succeed");

    let stored = fetch_asset_fee_tier(&svm, &asset_fee_tier);
    assert_eq!(stored.mint, mint);
    assert_eq!(stored.threshold, TIER_THRESHOLD);
    assert_eq!(stored.below_bps, TIER_BELOW_BPS);
    assert_eq!(stored.above_or_equal_bps, TIER_ABOVE_BPS);
    assert_eq!(stored.bump, bump);

    let ev = extract_anchor_event::<sra_handler::events::AssetFeeTierUpdated>(&meta.logs)
        .expect("AssetFeeTierUpdated event must be emitted");
    assert_eq!(ev.mint, mint);
    assert_eq!(ev.threshold, TIER_THRESHOLD);
    assert_eq!(ev.below_bps, TIER_BELOW_BPS);
    assert_eq!(ev.above_or_equal_bps, TIER_ABOVE_BPS);
}

#[test]
fn set_asset_fee_tier_requires_protocol_fee_config() {
    // EVM `ProtocolFeeNotSet` parity: a tier without a protocol fee config is
    // meaningless — the constraint surfaces as AccountNotInitialized.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        Pubkey::new_unique(),
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "tier without a protocol fee config must be rejected",
    );
    assert_eq!(err, anchor_error(ErrorCode::AccountNotInitialized));
}

#[test]
fn set_asset_fee_tier_bps_over_max_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);

    // Either bracket over the cap must fail (EVM checks both).
    for (below, above) in [
        (MAX_FEE_BPS + 1, TIER_ABOVE_BPS),
        (TIER_BELOW_BPS, MAX_FEE_BPS + 1),
    ] {
        let ix = set_asset_fee_tier_ix(
            &payer.pubkey(),
            &admin.pubkey(),
            Pubkey::new_unique(),
            TIER_THRESHOLD,
            below,
            above,
        );
        let err = send_ix_expect_err(
            &mut svm,
            ix,
            &payer,
            &[&admin],
            "tier bps over MAX_FEE_BPS must be rejected",
        );
        assert_eq!(err, custom_error(SraError::FeeTooHigh));
    }
}

#[test]
fn set_asset_fee_tier_upsert_updates() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let mint = Pubkey::new_unique();
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&mint);

    for threshold in [TIER_THRESHOLD, TIER_THRESHOLD * 2] {
        let ix = set_asset_fee_tier_ix(
            &payer.pubkey(),
            &admin.pubkey(),
            mint,
            threshold,
            TIER_BELOW_BPS,
            TIER_ABOVE_BPS,
        );
        send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier upsert should succeed");
    }

    assert_eq!(
        fetch_asset_fee_tier(&svm, &asset_fee_tier).threshold,
        TIER_THRESHOLD * 2
    );
}

#[test]
fn set_asset_fee_tier_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_with_protocol_fee(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &attacker.pubkey(),
        Pubkey::new_unique(),
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not set an asset fee tier",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

#[test]
fn remove_asset_fee_tier_closes_pda() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let mint = Pubkey::new_unique();
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&mint);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier set should succeed");

    let ix = remove_asset_fee_tier_ix(&admin.pubkey(), mint);
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("remove_asset_fee_tier should succeed");

    let closed = svm.get_account(&asset_fee_tier);
    assert!(
        closed.is_none() || closed.unwrap().lamports == 0,
        "asset_fee_tier PDA must be closed"
    );

    let ev = extract_anchor_event::<sra_handler::events::AssetFeeTierRemoved>(&meta.logs)
        .expect("AssetFeeTierRemoved event must be emitted");
    assert_eq!(ev.mint, mint);
}

// --------------------------------------------------------------------------
// 2-step admin transfer
// --------------------------------------------------------------------------

#[test]
fn set_pending_admin_happy_path() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let (fee_config, _) = derive_fee_config_pda();
    let new_pending = Pubkey::new_unique();

    let ix = set_pending_admin_ix(&admin.pubkey(), new_pending);
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&admin])
        .expect("set_pending_admin should succeed");

    assert_eq!(
        fetch_fee_config(&svm, &fee_config).pending_admin,
        new_pending
    );

    let ev = extract_anchor_event::<sra_handler::events::PendingAdminSet>(&meta.logs)
        .expect("PendingAdminSet event must be emitted");
    assert_eq!(ev.admin, admin.pubkey());
    assert_eq!(ev.pending_admin, new_pending);
}

#[test]
fn set_pending_admin_default_rejected() {
    // EVM parity: setPendingAdmin(address(0)) reverts InvalidAddress.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);

    let ix = set_pending_admin_ix(&admin.pubkey(), Pubkey::default());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "default pending admin must be rejected",
    );
    assert_eq!(err, custom_error(SraError::InvalidPendingAdmin));
}

#[test]
fn set_pending_admin_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_fee_config(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = set_pending_admin_ix(&attacker.pubkey(), Pubkey::new_unique());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not set a pending admin",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

#[test]
fn accept_admin_by_pending_transfers() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let new_admin = funded_keypair(&mut svm, SOL);
    let (fee_config, _) = derive_fee_config_pda();

    let ix = set_pending_admin_ix(&admin.pubkey(), new_admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("set_pending_admin should succeed");

    let ix = accept_admin_ix(&new_admin.pubkey());
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&new_admin])
        .expect("accept_admin should succeed");

    let stored = fetch_fee_config(&svm, &fee_config);
    assert_eq!(stored.admin, new_admin.pubkey());
    assert_eq!(stored.pending_admin, Pubkey::default());

    let ev = extract_anchor_event::<sra_handler::events::AdminTransferred>(&meta.logs)
        .expect("AdminTransferred event must be emitted");
    assert_eq!(ev.old_admin, admin.pubkey());
    assert_eq!(ev.new_admin, new_admin.pubkey());
}

#[test]
fn accept_admin_by_non_pending_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let new_admin = funded_keypair(&mut svm, SOL);
    let interloper = funded_keypair(&mut svm, SOL);

    let ix = set_pending_admin_ix(&admin.pubkey(), new_admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("set_pending_admin should succeed");

    let ix = accept_admin_ix(&interloper.pubkey());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&interloper],
        "only the pending admin may accept",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

#[test]
fn old_admin_loses_rights_after_transfer() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let new_admin = funded_keypair(&mut svm, SOL);

    let ix = set_pending_admin_ix(&admin.pubkey(), new_admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("set_pending_admin should succeed");
    let ix = accept_admin_ix(&new_admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&new_admin]).expect("accept_admin should succeed");

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS,
        false,
        false,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&admin],
        "the old admin must lose admin rights after the transfer",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

// --------------------------------------------------------------------------
// Lifecycle + boundary pins
// --------------------------------------------------------------------------

#[test]
fn fee_bps_max_boundary_succeeds() {
    // The cap is inclusive on both surfaces (EVM `<= MAX_FEE_BPS`).
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let mint = Pubkey::new_unique();

    let ix = set_protocol_fee_ix(&payer.pubkey(), &admin.pubkey(), MAX_FEE_BPS, false, false);
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("default_fee_bps == MAX must be accepted");

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        MAX_FEE_BPS,
        MAX_FEE_BPS,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier bps == MAX must be accepted");

    let (protocol_fee_config, _) = derive_protocol_fee_pda();
    assert_eq!(
        fetch_protocol_fee_config(&svm, &protocol_fee_config).default_fee_bps,
        MAX_FEE_BPS
    );
}

#[test]
fn protocol_fee_remove_then_set_round_trips() {
    // PDA existence == isSet: close → re-create must work and re-stamp fields.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let (protocol_fee_config, bump) = derive_protocol_fee_pda();

    let ix = remove_protocol_fee_ix(&admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("remove should succeed");

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS + 1,
        true,
        true,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("re-create after close must succeed");

    let stored = fetch_protocol_fee_config(&svm, &protocol_fee_config);
    assert_eq!(stored.default_fee_bps, DEFAULT_FEE_BPS + 1);
    assert!(stored.allow_override);
    assert!(stored.allow_sponsored);
    assert_eq!(stored.bump, bump);
}

#[test]
fn remove_protocol_fee_returns_rent_to_admin() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let (protocol_fee_config, _) = derive_protocol_fee_pda();

    let rent = svm
        .get_account(&protocol_fee_config)
        .expect("config exists")
        .lamports;
    let admin_before = svm
        .get_account(&admin.pubkey())
        .expect("admin exists")
        .lamports;

    let ix = remove_protocol_fee_ix(&admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("remove should succeed");

    let admin_after = svm
        .get_account(&admin.pubkey())
        .expect("admin exists")
        .lamports;
    assert_eq!(
        admin_after,
        admin_before + rent,
        "closed PDA rent must land on the admin"
    );
}

#[test]
fn asset_fee_tier_survives_and_revives_across_protocol_fee_removal() {
    // EVM parity: removing protocolFeeConfig leaves assetFeeTiers dangling
    // (inert while unconfigured) and re-registration revives them unchanged.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let mint = Pubkey::new_unique();
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&mint);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier set should succeed");

    let ix = remove_protocol_fee_ix(&admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("remove config should succeed");
    assert!(
        svm.get_account(&asset_fee_tier).is_some(),
        "tier must survive the config removal (dangling, inert)"
    );

    let ix = set_protocol_fee_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        DEFAULT_FEE_BPS,
        false,
        false,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("re-register should succeed");

    let revived = fetch_asset_fee_tier(&svm, &asset_fee_tier);
    assert_eq!(revived.threshold, TIER_THRESHOLD);
    assert_eq!(revived.below_bps, TIER_BELOW_BPS);
    assert_eq!(revived.above_or_equal_bps, TIER_ABOVE_BPS);
}

#[test]
fn remove_asset_fee_tier_without_protocol_fee_config_succeeds() {
    // Deliberate divergence pin: tier cleanup (and rent recovery) stays
    // possible after the app's config is removed — EVM strands such tiers.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let mint = Pubkey::new_unique();
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&mint);

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier set should succeed");

    let ix = remove_protocol_fee_ix(&admin.pubkey());
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("remove config should succeed");

    let ix = remove_asset_fee_tier_ix(&admin.pubkey(), mint);
    send_ix(&mut svm, ix, &payer, &[&admin])
        .expect("dangling tier must remain removable without a live config");

    let closed = svm.get_account(&asset_fee_tier);
    assert!(closed.is_none() || closed.unwrap().lamports == 0);
}

#[test]
fn remove_asset_fee_tier_non_admin_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_with_protocol_fee(&mut svm, &payer);
    let attacker = funded_keypair(&mut svm, SOL);
    let mint = Pubkey::new_unique();

    let ix = set_asset_fee_tier_ix(
        &payer.pubkey(),
        &admin.pubkey(),
        mint,
        TIER_THRESHOLD,
        TIER_BELOW_BPS,
        TIER_ABOVE_BPS,
    );
    send_ix(&mut svm, ix, &payer, &[&admin]).expect("tier set should succeed");

    let ix = remove_asset_fee_tier_ix(&attacker.pubkey(), mint);
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&attacker],
        "non-admin must not remove an asset fee tier",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

#[test]
fn accept_admin_without_pending_rejected() {
    // EVM NoPendingAdmin analog: pending_admin == Pubkey::default() is
    // unsatisfiable (no known key), so any signer fails ConstraintHasOne.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let _admin = setup_fee_config(&mut svm, &payer);
    let interloper = funded_keypair(&mut svm, SOL);

    let ix = accept_admin_ix(&interloper.pubkey());
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&interloper],
        "accept without a pending admin must fail",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
}

#[test]
fn set_pending_admin_overwrites_previous() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let admin = setup_fee_config(&mut svm, &payer);
    let (fee_config, _) = derive_fee_config_pda();

    let first = Pubkey::new_unique();
    let second = Pubkey::new_unique();
    for pending in [first, second] {
        let ix = set_pending_admin_ix(&admin.pubkey(), pending);
        send_ix(&mut svm, ix, &payer, &[&admin]).expect("set_pending_admin should succeed");
    }

    assert_eq!(fetch_fee_config(&svm, &fee_config).pending_admin, second);
}
