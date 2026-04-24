mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::errors::SraError;
use sra_handler::instructions::source::update_mint_route::UpdateMintRouteParams;

#[test]
fn update_mint_route_slippage_updates() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    let params = UpdateMintRouteParams {
        allowed_slippage_bps: Some(777),
        ..Default::default()
    };
    let ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        None,
        params,
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner])
        .expect("slippage update should succeed");

    let route = fetch_mint_route(&svm, &fx.accounts.mint_route);
    assert_eq!(route.allowed_slippage_bps, 777);
    assert_eq!(
        route.target_token, fx.target_token,
        "target_token must be untouched when only slippage is updated"
    );
    assert!(!route.paused, "paused must be untouched");
}

#[test]
fn update_mint_route_target_token_valid_address_book_updates() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // Enable a second address-book entry for a new target_token.
    let new_target_token = [0x99; 32];
    let new_ab = setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.sra_config,
        fx.target_chain_id,
        new_target_token,
        18,
    );

    let params = UpdateMintRouteParams {
        target_token: Some(new_target_token),
        ..Default::default()
    };
    let ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        Some(&new_ab),
        params,
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner])
        .expect("target_token update against enabled AB should succeed");

    let route = fetch_mint_route(&svm, &fx.accounts.mint_route);
    assert_eq!(route.target_token, new_target_token);
}

#[test]
fn update_mint_route_target_token_disabled_address_book_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // Disabled (tombstoned) entry for a new token.
    let new_target_token = [0x55; 32];
    let disabled_ab = setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.sra_config,
        fx.target_chain_id,
        new_target_token,
        0,
    );

    let params = UpdateMintRouteParams {
        target_token: Some(new_target_token),
        ..Default::default()
    };
    let ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        Some(&disabled_ab),
        params,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&fx.owner],
        "target_token update against disabled AB must be rejected",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAddressBook.into()),
        )
    );

    // Previous state preserved.
    let route = fetch_mint_route(&svm, &fx.accounts.mint_route);
    assert_eq!(route.target_token, fx.target_token);
}

#[test]
fn update_mint_route_target_token_missing_address_book_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // No address_book_entry account passed with a target_token update.
    let params = UpdateMintRouteParams {
        target_token: Some([0x77; 32]),
        ..Default::default()
    };
    let ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        None,
        params,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&fx.owner],
        "target_token update without AB entry must be rejected",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAddressBook.into()),
        )
    );
}

#[test]
fn update_mint_route_pause_flips_paused() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    let pause_ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        None,
        UpdateMintRouteParams {
            paused: Some(true),
            ..Default::default()
        },
    );
    send_ix(&mut svm, pause_ix, &fx.payer, &[&fx.owner]).expect("pause should succeed");
    assert!(fetch_mint_route(&svm, &fx.accounts.mint_route).paused);

    let unpause_ix = update_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        None,
        UpdateMintRouteParams {
            paused: Some(false),
            ..Default::default()
        },
    );
    send_ix(&mut svm, unpause_ix, &fx.payer, &[&fx.owner]).expect("unpause should succeed");
    assert!(!fetch_mint_route(&svm, &fx.accounts.mint_route).paused);
}

#[test]
fn update_mint_route_non_owner_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = update_mint_route_ix(
        &attacker.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        None,
        UpdateMintRouteParams {
            paused: Some(true),
            ..Default::default()
        },
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&attacker],
        "non-owner must not update mint route",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32),
        )
    );
}
