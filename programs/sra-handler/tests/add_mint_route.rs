mod common;

use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::{
    constants::{MAX_ACTION_IDXS, MAX_SLIPPAGE_BPS},
    errors::SraError,
};
use sra_handler::instructions::source::add_mint_route::AddMintRouteParams;

#[test]
fn add_mint_route_happy_path() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);

    let (expected_mint_route, expected_mint_route_bump) =
        derive_mint_route_pda(&fx.sra_config, &fx.source_mint);
    let (expected_mint_nonce, expected_mint_nonce_bump) =
        derive_mint_nonce_pda(&fx.sra_config, &fx.source_mint);

    let params = AddMintRouteParams {
        action_idxs: vec![3, 7],
        target_token: fx.target_token,
        allowed_slippage_bps: 50,
    };

    let ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        params.clone(),
    );

    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("add_mint_route should succeed");

    let route = fetch_mint_route(&svm, &expected_mint_route);
    assert_eq!(route.sra_config, fx.sra_config);
    assert_eq!(route.source_mint, fx.source_mint);
    assert_eq!(route.action_idxs, params.action_idxs);
    assert_eq!(route.target_token, fx.target_token);
    assert_eq!(route.allowed_slippage_bps, params.allowed_slippage_bps);
    assert!(!route.paused, "new routes must start unpaused");
    assert_eq!(route.bump, expected_mint_route_bump);

    let nonce = fetch_mint_nonce(&svm, &expected_mint_nonce);
    assert_eq!(nonce.nonce, 0, "fresh MintNonce starts at 0");
    assert_eq!(nonce.bump, expected_mint_nonce_bump);

    // Vault ATAs exist and are owned by the SPL Token program with zero balance.
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.adapter_custody), 0);
}

#[test]
fn add_mint_route_too_many_action_idxs_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);

    let too_many = (0..=MAX_ACTION_IDXS as u64).collect::<Vec<_>>();
    let params = AddMintRouteParams {
        action_idxs: too_many,
        target_token: fx.target_token,
        allowed_slippage_bps: 0,
    };
    let ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        params,
    );

    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&fx.owner],
        "action_idxs > MAX must fail",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::TooManyActionIdxs.into()),
        )
    );
}

#[test]
fn add_mint_route_disabled_address_book_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let sra_config = init_sra(&mut svm, &payer, &owner);
    let source_mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let target_chain_id = sample_config().target_chain_id;
    let target_token = [0xCD; 32];

    // Tombstoned entry (decimals = 0) — set directly.
    setup_address_book_entry(
        &mut svm,
        &payer,
        &owner,
        &sra_config,
        target_chain_id,
        target_token,
        0,
    );

    let adapter_program = solana_sdk::pubkey::Pubkey::new_unique();
    let accounts = MintRouteAccounts::derive(
        &sra_config,
        &source_mint,
        target_chain_id,
        &target_token,
        &adapter_program,
    );

    let ix = add_mint_route_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &accounts,
        AddMintRouteParams {
            action_idxs: vec![0],
            target_token,
            allowed_slippage_bps: 0,
        },
    );

    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &payer,
        &[&owner],
        "tombstoned address_book entry must be rejected",
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
fn add_mint_route_duplicate_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);

    fx.add_default_route(&mut svm);

    // Second call with the same (sra, mint) must fail because mint_route PDA is
    // `init` (not `init_if_needed`).
    let ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        fx.default_params(),
    );
    let result = send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]);
    assert!(
        result.is_err(),
        "duplicate add_mint_route for same (sra, mint) must fail",
    );
}

#[test]
fn add_mint_route_slippage_over_max_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);

    let params = AddMintRouteParams {
        action_idxs: vec![0],
        target_token: fx.target_token,
        allowed_slippage_bps: MAX_SLIPPAGE_BPS + 1,
    };
    let ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        params,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&fx.owner],
        "slippage above MAX must be rejected",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidSlippage.into()),
        )
    );
}

#[test]
fn add_mint_route_non_owner_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &attacker.pubkey(),
        &fx.accounts,
        fx.default_params(),
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&attacker],
        "non-owner must not be able to add a mint route",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(anchor_lang::error::ErrorCode::ConstraintHasOne as u32),
        )
    );
}
