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

fn mr_params(pause: Option<bool>, adapters: Option<Vec<Pubkey>>) -> ManageRouteParams {
    ManageRouteParams { pause, adapters }
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

    let ix = manage_route_ix(&owner.pubkey(), &route, mr_params(Some(true), None), &[]);
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("pause should succeed");

    let after = fetch_route(&svm, &route);
    assert!(after.paused, "paused must be set");
    assert_eq!(
        after.allowed_adapters,
        vec![ADAPTER_PROGRAM_ID],
        "pause with adapters=None must leave the list unchanged"
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
        manage_route_ix(&owner.pubkey(), &route, mr_params(Some(true), None), &[]),
        &payer,
        &[&owner],
    )
    .expect("pause should succeed");
    assert!(fetch_route(&svm, &route).paused);

    send_ix(
        &mut svm,
        manage_route_ix(&owner.pubkey(), &route, mr_params(Some(false), None), &[]),
        &payer,
        &[&owner],
    )
    .expect("unpause should succeed");
    assert!(!fetch_route(&svm, &route).paused, "paused must be cleared");
}

// --------------------------------------------------------------------------
// adapter set (whole-list replace, EVM _installAdapters parity)
// --------------------------------------------------------------------------

#[test]
fn manage_route_set_adapters_replaces_list() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [3u8; 32]);

    let extra = deploy_extra_adapter(&mut svm);
    let new_set = vec![ADAPTER_PROGRAM_ID, extra];
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, Some(new_set.clone())),
        &new_set,
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("set adapters should succeed");

    assert_eq!(fetch_route(&svm, &route).allowed_adapters, new_set);
}

#[test]
fn manage_route_set_adapters_can_drop_one() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // Route starts with two adapters; replace with a list omitting the first —
    // the EVM way to drop a compromised adapter (uninstall + reinstall the set).
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
        mr_params(None, Some(vec![extra])),
        &[extra],
    );
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("replace adapters should succeed");

    assert_eq!(fetch_route(&svm, &route).allowed_adapters, vec![extra]);
}

#[test]
fn manage_route_clear_adapters() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [5u8; 32]);

    // An empty replacement set clears the allowlist (EVM uninstall parity).
    let ix = manage_route_ix(&owner.pubkey(), &route, mr_params(None, Some(vec![])), &[]);
    send_ix(&mut svm, ix, &payer, &[&owner]).expect("clearing adapters should succeed");

    assert!(fetch_route(&svm, &route).allowed_adapters.is_empty());
}

#[test]
fn manage_route_adapters_edit_preserves_pause() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [12u8; 32]);

    // pause and adapters are independent: an adapters-only edit (pause = None)
    // must leave a previously-set paused flag intact.
    send_ix(
        &mut svm,
        manage_route_ix(&owner.pubkey(), &route, mr_params(Some(true), None), &[]),
        &payer,
        &[&owner],
    )
    .expect("pause should succeed");

    let extra = deploy_extra_adapter(&mut svm);
    let new_set = vec![ADAPTER_PROGRAM_ID, extra];
    send_ix(
        &mut svm,
        manage_route_ix(
            &owner.pubkey(),
            &route,
            mr_params(None, Some(new_set.clone())),
            &new_set,
        ),
        &payer,
        &[&owner],
    )
    .expect("adapters-only edit should succeed");

    let after = fetch_route(&svm, &route);
    assert!(after.paused, "adapters-only edit must not clear paused");
    assert_eq!(after.allowed_adapters, new_set);
}

#[test]
fn manage_route_non_executable_adapter_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [6u8; 32]);

    // A pubkey with no deployed program → empty account → not executable.
    let bogus = Pubkey::new_unique();
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, Some(vec![bogus])),
        &[bogus],
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("non-executable adapter must be rejected")
        .err;
    assert_eq!(err, custom_error(SraError::InvalidAdapterProgram));

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
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [7u8; 32]);

    // A replacement set of MAX_ADAPTERS + 1 distinct adapters exceeds the bound.
    let set: Vec<_> = (0..=MAX_ADAPTERS)
        .map(|_| deploy_extra_adapter(&mut svm))
        .collect();
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, Some(set.clone())),
        &set,
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
fn manage_route_duplicate_adapters_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 10 * SOL);
    let owner = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [8u8; 32]);

    // The replacement set is a set: a repeated entry is rejected.
    let extra = deploy_extra_adapter(&mut svm);
    let dup = vec![extra, extra];
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(None, Some(dup.clone())),
        &dup,
    );
    let err = send_ix(&mut svm, ix, &payer, &[&owner])
        .expect_err("a duplicate in the replacement set must be rejected")
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
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [9u8; 32]);

    // Attacker signs as the owner account; `has_one = owner` rejects the mismatch.
    let ix = manage_route_ix(&attacker.pubkey(), &route, mr_params(Some(true), None), &[]);
    let err = send_ix(&mut svm, ix, &payer, &[&attacker])
        .expect_err("non-owner must be rejected")
        .err;
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));

    assert!(
        !fetch_route(&svm, &route).paused,
        "rejected call must not mutate the route"
    );
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
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [10u8; 32]);

    let extra = deploy_extra_adapter(&mut svm);
    let new_set = vec![ADAPTER_PROGRAM_ID, extra];
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(Some(true), Some(new_set.clone())),
        &new_set,
    );
    let meta = send_ix_with_meta(&mut svm, ix, &payer, &[&owner]).expect("manage_route succeeds");

    let ev = extract_anchor_event::<sra_handler::events::RouteManaged>(&meta.logs)
        .expect("RouteManaged event must be emitted");
    assert_eq!(ev.route, route);
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(
        ev.allowed_adapters, new_set,
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
    let route = create_sample_route(&mut svm, &payer, &owner, mint, [11u8; 32]);

    // Planted directly (finalize_fill is a later step); manage_route must not touch it.
    let fill_receipt = seed_fill_receipt_account(&mut svm, &route, [0xCDu8; 32], 5_000, 254);
    let before = svm
        .get_account(&fill_receipt)
        .expect("planted receipt exists")
        .data;

    let extra = deploy_extra_adapter(&mut svm);
    let new_set = vec![ADAPTER_PROGRAM_ID, extra];
    let ix = manage_route_ix(
        &owner.pubkey(),
        &route,
        mr_params(Some(true), Some(new_set.clone())),
        &new_set,
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
