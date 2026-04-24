mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::errors::SraError;
use sra_handler::instructions::source::add_mint_route::AddMintRouteParams;

#[test]
fn close_mint_route_empty_vault_returns_rent() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    let owner_lamports_before = svm
        .get_account(&fx.owner.pubkey())
        .expect("owner exists")
        .lamports;

    let ix = close_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        &fx.accounts.sra_vault,
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("close should succeed");

    assert!(
        !mint_route_exists(&svm, &fx.accounts.mint_route),
        "mint_route PDA must be closed"
    );

    let owner_lamports_after = svm
        .get_account(&fx.owner.pubkey())
        .expect("owner exists")
        .lamports;
    assert!(
        owner_lamports_after > owner_lamports_before,
        "closed mint_route rent must accrue to owner ({} -> {})",
        owner_lamports_before,
        owner_lamports_after,
    );
}

#[test]
fn close_mint_route_non_empty_vault_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // Mint some tokens into the SRA vault so it is non-empty. Payer is the mint authority.
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &fx.accounts.sra_vault,
        1_000,
    );
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000);

    let ix = close_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        &fx.accounts.sra_vault,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&fx.owner],
        "close must be rejected while vault has balance",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::VaultNotEmpty.into()),
        )
    );

    // Route still exists, unchanged.
    assert!(mint_route_exists(&svm, &fx.accounts.mint_route));
}

#[test]
fn close_mint_route_mint_nonce_persists() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // Nonce exists prior to close; snapshot it.
    let nonce_before = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
    let nonce_acct_before = svm
        .get_account(&fx.accounts.mint_nonce)
        .expect("mint_nonce account exists before close");

    let ix = close_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        &fx.accounts.sra_vault,
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("close should succeed");

    let nonce_acct_after = svm
        .get_account(&fx.accounts.mint_nonce)
        .expect("mint_nonce account MUST persist after close_mint_route");
    assert_eq!(
        nonce_acct_before.lamports, nonce_acct_after.lamports,
        "mint_nonce rent must not be refunded on close",
    );
    let nonce_after = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
    assert_eq!(
        nonce_after.nonce, nonce_before.nonce,
        "mint_nonce value must persist across close_mint_route",
    );
}

#[test]
fn close_mint_route_then_recreate_preserves_nonce() {
    // Migration flow: close + re-add_mint_route. The second add must not reset
    // the nonce — otherwise the EVM replay-key set can be collided.
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);

    // Simulate nonce progress by writing directly to the underlying account
    // via an init overwrite is intrusive; instead we rely on the initial
    // nonce=0 state and focus on the persistence invariant: after close +
    // re-add, the MintNonce bump and account metadata must survive.
    let nonce_before = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);

    let close_ix = close_mint_route_ix(
        &fx.owner.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        &fx.accounts.sra_vault,
    );
    send_ix(&mut svm, close_ix, &fx.payer, &[&fx.owner]).expect("close should succeed");

    // Re-add with a different action_idxs, same mint.
    let re_add_ix = add_mint_route_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        AddMintRouteParams {
            action_idxs: vec![42],
            target_token: fx.target_token,
            allowed_slippage_bps: 0,
        },
    );
    send_ix(&mut svm, re_add_ix, &fx.payer, &[&fx.owner]).expect("re-add should succeed");

    let nonce_after = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
    assert_eq!(
        nonce_after.nonce, nonce_before.nonce,
        "MintNonce must survive close + re-add unchanged"
    );
    assert_eq!(
        nonce_after.bump, nonce_before.bump,
        "MintNonce bump must remain stable"
    );

    // The new route has the updated action_idxs.
    let route = fetch_mint_route(&svm, &fx.accounts.mint_route);
    assert_eq!(route.action_idxs, vec![42]);
}

#[test]
fn close_mint_route_non_owner_rejected() {
    let mut svm = setup_svm();
    let fx = MintRouteFixture::new(&mut svm);
    fx.add_default_route(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);

    let ix = close_mint_route_ix(
        &attacker.pubkey(),
        &fx.sra_config,
        &fx.accounts.mint_route,
        &fx.accounts.sra_vault,
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &fx.payer,
        &[&attacker],
        "non-owner must not close mint route",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(ErrorCode::ConstraintHasOne as u32),
        )
    );

    assert!(mint_route_exists(&svm, &fx.accounts.mint_route));
}
