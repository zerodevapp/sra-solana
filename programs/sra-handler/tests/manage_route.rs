mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};
use sra_common::{constants::MAX_ADAPTERS, errors::SraError, types::ManageRouteParams};

fn custom_error(err: SraError) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(err.into()))
}

fn anchor_error(code: ErrorCode) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(code as u32))
}

fn mr_params(
    pause: Option<bool>,
    add_adapters: Vec<Pubkey>,
    remove_adapters: Vec<Pubkey>,
) -> ManageRouteParams {
    ManageRouteParams {
        pause,
        add_adapters,
        remove_adapters,
    }
}

// --------------------------------------------------------------------------
// pause / unpause
// --------------------------------------------------------------------------

#[test]
fn manage_route_pause_sets_paused() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [1u8; 32]);

    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(Some(true), vec![], vec![]),
        &[],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("pause should succeed");

    let after = fetch_route(&svm, &route);
    assert!(after.paused, "paused must be set");
    assert_eq!(
        after.allowed_adapters,
        vec![ADAPTER_PROGRAM_ID],
        "pause must not touch the adapter list"
    );
}

#[test]
fn manage_route_unpause_clears_paused() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [2u8; 32]);

    send_ix(
        &mut svm,
        manage_route_ix(
            &owner.pubkey(),
            &route,
            mr_params(Some(true), vec![], vec![]),
            &[],
        ),
        &payer,
        &[&owner],
    )
    .expect("pause should succeed");
    assert!(fetch_route(&svm, &route).paused);

    send_ix(
        &mut svm,
        manage_route_ix(
            &owner.pubkey(),
            &route,
            mr_params(Some(false), vec![], vec![]),
            &[],
        ),
        &payer,
        &[&owner],
    )
    .expect("unpause should succeed");
    assert!(!fetch_route(&svm, &route).paused, "paused must be cleared");
}

// --------------------------------------------------------------------------
// adapter add / remove
// --------------------------------------------------------------------------

#[test]
fn manage_route_add_adapter() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [3u8; 32]);

    let extra = deploy_extra_adapter(&mut svm);
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, vec![extra], vec![]),
        &[extra],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("add adapter should succeed");

    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![ADAPTER_PROGRAM_ID, extra],
        "new adapter appended to the list"
    );
}

#[test]
fn manage_route_remove_adapter() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // Create a route that starts with two adapters so removal is surgical.
    let extra = deploy_extra_adapter(&mut svm);
    let mut params = sample_create_route_params(mint, [4u8; 32]);
    params.allowed_adapters = vec![ADAPTER_PROGRAM_ID, extra];
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    send_ix(
        &mut svm,
        create_route_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            params,
            &[ADAPTER_PROGRAM_ID, extra],
        ),
        &payer,
        &[&owner],
    )
    .expect("create_route with two adapters should succeed");

    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, vec![], vec![ADAPTER_PROGRAM_ID]),
        &[],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("remove adapter should succeed");

    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![extra],
        "only the named adapter is removed"
    );
}

#[test]
fn manage_route_add_non_executable_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [5u8; 32]);

    // A pubkey with no deployed program → empty account → not executable.
    let bogus = Pubkey::new_unique();
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, vec![bogus], vec![]),
        &[bogus],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("non-executable adapter must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidAdapterProgram));

    // Rejection must preserve route state.
    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![ADAPTER_PROGRAM_ID]
    );
}

#[test]
fn manage_route_too_many_adapters_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [6u8; 32]);

    // Route already holds one adapter; adding MAX_ADAPTERS *distinct* executable
    // adapters overflows the bound (1 + 8 = 9 > 8). Distinct, so the failure is
    // the length bound, not the dedup or executable checks.
    let adds: Vec<_> = (0..MAX_ADAPTERS)
        .map(|_| deploy_extra_adapter(&mut svm))
        .collect();
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, adds.clone(), vec![]),
        &adds,
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("exceeding MAX_ADAPTERS must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::TooManyAdapters));

    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![ADAPTER_PROGRAM_ID]
    );
}

#[test]
fn manage_route_add_already_present_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [10u8; 32]);

    // The allowlist is a set: re-adding an adapter already present is rejected.
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, vec![ADAPTER_PROGRAM_ID], vec![]),
        &[ADAPTER_PROGRAM_ID],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("re-adding an existing adapter must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::DuplicateAdapter));

    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![ADAPTER_PROGRAM_ID]
    );
}

#[test]
fn manage_route_intra_batch_duplicate_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [11u8; 32]);

    // The same new adapter listed twice in one call is rejected.
    let extra = deploy_extra_adapter(&mut svm);
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, vec![extra, extra], vec![]),
        &[extra, extra],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("a duplicate within add_adapters must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::DuplicateAdapter));

    assert_eq!(
        fetch_route(&svm, &route).allowed_adapters,
        vec![ADAPTER_PROGRAM_ID]
    );
}

// --------------------------------------------------------------------------
// authorization
// --------------------------------------------------------------------------

#[test]
fn manage_route_non_owner_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let attacker = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [7u8; 32]);

    // Attacker signs as the owner account; `has_one = owner` rejects the mismatch.
    let ix = manage_route_ix(
        &attacker.pubkey(),
        &route,
        mr_params(Some(true), vec![], vec![]),
        &[],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&attacker])
        .expect_err("non-owner must be rejected")
        .err;
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));

    let after = fetch_route(&svm, &route);
    assert!(!after.paused, "rejected call must not mutate the route");
}

// --------------------------------------------------------------------------
// event + state-machine independence
// --------------------------------------------------------------------------

#[test]
fn manage_route_emits_route_managed() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [8u8; 32]);

    let extra = deploy_extra_adapter(&mut svm);
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(Some(true), vec![extra], vec![]),
        &[extra],
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&owner]).expect("manage_route succeeds");

    let ev = extract_anchor_event::<sra_handler::events::RouteManaged>(&meta.logs)
        .expect("RouteManaged event must be emitted");
    assert_eq!(ev.route, route);
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(
        ev.allowed_adapters,
        vec![ADAPTER_PROGRAM_ID, extra],
        "event carries the post-write adapter list"
    );
    assert!(ev.paused, "event carries the post-write paused flag");
}

#[test]
fn manage_route_does_not_affect_finalized_receipts() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [9u8; 32]);

    // A finalized fill exists for this route (planted directly — finalize_fill
    // is a later step). manage_route must not touch the FillReceipt state machine.
    let fill_receipt = seed_fill_receipt_account(&mut svm, &route, [0xCDu8; 32], 5_000, 254);
    let before = svm
        .get_account(&fill_receipt)
        .expect("planted receipt exists")
        .data;

    let extra = deploy_extra_adapter(&mut svm);
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(Some(true), vec![extra], vec![]),
        &[extra],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("manage_route succeeds");

    let after = svm
        .get_account(&fill_receipt)
        .expect("receipt still exists")
        .data;
    assert_eq!(
        before, after,
        "FillReceipt must be untouched by manage_route"
    );
}
