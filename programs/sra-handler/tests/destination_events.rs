//! Destination event-stream reconciliation — the operational *indexer contract*.
//!
//! The per-instruction suites each assert one event in isolation, often from a
//! planted receipt. This suite proves the complementary property the monitoring
//! pipeline depends on: the **emitted event stream alone** — with no account
//! diffing — lets an indexer reconcile a fill across programs and instructions by
//! joining on `(route, fill_id)`.
//!
//! Chain reconciled:
//! ```text
//!   AcrossFillVerified (adapter) -> FillFinalized (handler)
//!                                -> ActionsExecuted | ActionsFailed -> ActionsRetried
//!                                -> FundsRecovered
//! ```
//! The destination decomposes into more events than the EVM handler (which emits a
//! single action-executed event per fill); this proves the decomposed stream still
//! carries the full identity + amount lineage parity requires, plus finer per-fill
//! granularity. The events key on `(route, fill_id)` independent of the action
//! variant, so SplTransfer configs exercise the join fully (the VerifiedCpi
//! emission and hard-revert/no-phantom paths are covered by
//! `verified_cpi_actions` / `actions_lifecycle`).
//!
//! Every test drives the genuine path through real instructions, captures each
//! transaction's logs, and runs the reconciliation over event payloads ONLY —
//! never over `fetch_token_balance` / `status`. Trailing account-state lines are
//! sanity checks, explicitly separate from the reconciliation.

mod common;

use common::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_across_adapter::events::AcrossFillVerified;
use sra_common::types::{CreateRouteParams, ExecutionStatus, SolanaAction};
use sra_handler::events::{
    ActionsExecuted, ActionsFailed, ActionsRetried, FillFinalized, FundsRecovered,
};

const AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const DECIMALS: u8 = 6;
/// Matches `sample_relay_data`'s fill deadline (the relay hash covers it).
const FILL_DEADLINE: u32 = 1_900_000_000;

/// A fill driven through the genuine verify + finalize path, with the two events
/// those legs emit captured from their transaction logs.
struct Driven {
    route: Pubkey,
    fill_id: [u8; 32],
    mint: Pubkey,
    owner: Keypair,
    payer: Keypair,
    execution_vault: Pubkey,
    verified: AcrossFillVerified,
    finalized: FillFinalized,
}

/// `create_route -> relayer fill -> verify_across_fill -> finalize_fill`, capturing
/// `AcrossFillVerified` (adapter) and `FillFinalized` (handler) from their logs.
fn drive_to_finalized(
    action_config: Vec<SolanaAction>,
    salt: [u8; 32],
) -> (litesvm::LiteSVM, Driven) {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);

    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: mint,
        app_id: [0x44u8; 32],
        action_config,
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
    };
    let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &salt);
    send_ix(
        &mut svm,
        create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            route_config_hash,
            params,
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .expect("create_route should succeed");

    // A relayer fills the route vault exactly as the deployed Across program does.
    let route_vault = derive_ata(&route, &mint);
    let relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let relayer = funded_keypair(&mut svm, 100 * SOL);
    let relayer_ata = create_ata(&mut svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(&mut svm, &payer, &payer, &mint, &relayer_ata, AMOUNT);
    send_ix(
        &mut svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            AMOUNT,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .expect("relayer fill should succeed");

    // verify_across_fill -> AcrossFillVerified (adapter program).
    let verify_meta = send_ix_with_meta(
        &mut svm,
        verify_across_fill_ix(&payer.pubkey(), &route, relay_hash, relay_data),
        &payer,
        &[],
    )
    .expect("verify_across_fill should succeed");
    let verified: AcrossFillVerified =
        extract_anchor_event(&verify_meta.logs).expect("AcrossFillVerified emitted");

    // finalize_fill -> FillFinalized (handler program).
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    let finalize_meta = send_ix_with_meta(
        &mut svm,
        finalize_fill_ix(
            &payer.pubkey(),
            &route,
            route_config_hash,
            &verified_fill,
            relay_hash,
            &mint,
        ),
        &payer,
        &[],
    )
    .expect("finalize_fill should succeed");
    let finalized: FillFinalized =
        extract_anchor_event(&finalize_meta.logs).expect("FillFinalized emitted");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);

    let driven = Driven {
        route,
        fill_id: relay_hash,
        mint,
        owner,
        payer,
        execution_vault,
        verified,
        finalized,
    };
    (svm, driven)
}

/// A single `SplTransfer` of 100% to `recipient`.
fn spl_config(recipient: Pubkey) -> Vec<SolanaAction> {
    vec![spl(recipient, 10_000)]
}

/// Reconcile the verify -> finalize legs from EVENT PAYLOADS ONLY (no account
/// reads): the cross-program events join on `(route, fill_id)` and the finalized
/// amount chains from the verified amount with no destination fee.
fn assert_verify_finalize_reconciles(d: &Driven) {
    assert_eq!(d.verified.route, d.route);
    assert_eq!(d.verified.fill_id, d.fill_id);
    assert_eq!(d.verified.mint, d.mint);
    assert_eq!(d.verified.source_chain_id, SOURCE_CHAIN_ID);
    assert_eq!(d.finalized.route, d.verified.route, "shared route join key");
    assert_eq!(
        d.finalized.fill_id, d.verified.fill_id,
        "shared fill_id join key"
    );
    assert_eq!(
        d.finalized.amount, d.verified.amount,
        "amount chains verify -> finalize"
    );
    assert_eq!(d.finalized.expected_amount, d.verified.expected_amount);
    assert_eq!(
        d.verified.expected_amount, AMOUNT,
        "expected_amount anchored to the gross fill (not a self-equal 0 == 0)"
    );
    assert_eq!(
        d.finalized.amount, AMOUNT,
        "full verified amount forwarded, no destination fee"
    );
    assert_eq!(d.finalized.execution_status, ExecutionStatus::Finalized);
}

#[test]
fn event_stream_reconciles_verify_finalize_execute_success() {
    let recipient = Pubkey::new_unique();
    let (mut svm, d) = drive_to_finalized(spl_config(recipient), [0x31; 32]);
    assert_verify_finalize_reconciles(&d);

    let recipient_ata = create_ata(&mut svm, &d.payer, &recipient, &d.mint);
    let meta = send_ix_with_meta(
        &mut svm,
        execute_actions_ix(
            &d.route,
            d.fill_id,
            spl_config(recipient),
            &d.mint,
            &[recipient_ata],
        ),
        &d.payer,
        &[],
    )
    .expect("execute_actions should succeed");
    let executed: ActionsExecuted =
        extract_anchor_event(&meta.logs).expect("ActionsExecuted emitted");
    // A success tx carries no ActionsFailed -> the indexer reads success cleanly.
    assert!(extract_anchor_event::<ActionsFailed>(&meta.logs).is_none());

    // Reconciliation (events only): the success leg joins the same fill.
    assert_eq!(executed.route, d.route);
    assert_eq!(executed.fill_id, d.fill_id);
    assert_eq!(
        executed.route, d.finalized.route,
        "actions join the finalized fill"
    );
    assert_eq!(executed.fill_id, d.finalized.fill_id);

    // Sanity (account state, NOT part of the reconciliation): funds delivered.
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &d.execution_vault), 0);
}

#[test]
fn event_stream_reconciles_actions_failed_then_retried() {
    let recipient = Pubkey::new_unique();
    let (mut svm, d) = drive_to_finalized(spl_config(recipient), [0x32; 32]);
    assert_verify_finalize_reconciles(&d);

    // Soft fail: recipient ATA missing -> ActionsFailed (funds preserved, Ok).
    let missing = derive_ata(&recipient, &d.mint);
    let fail_meta = send_ix_with_meta(
        &mut svm,
        execute_actions_ix(
            &d.route,
            d.fill_id,
            spl_config(recipient),
            &d.mint,
            &[missing],
        ),
        &d.payer,
        &[],
    )
    .expect("missing recipient ATA -> ActionsFailed (returns Ok)");
    let failed: ActionsFailed =
        extract_anchor_event(&fail_meta.logs).expect("ActionsFailed emitted");
    // A soft fail must not look like success to the indexer.
    assert!(extract_anchor_event::<ActionsExecuted>(&fail_meta.logs).is_none());

    // Fix the ATA out-of-band, then the owner retries -> ActionsRetried.
    let recipient_ata = create_ata(&mut svm, &d.payer, &recipient, &d.mint);
    let retry_meta = send_ix_with_meta(
        &mut svm,
        retry_actions_ix(
            &d.owner.pubkey(),
            &d.route,
            d.fill_id,
            spl_config(recipient),
            &d.mint,
            &[recipient_ata],
        ),
        &d.payer,
        &[&d.owner],
    )
    .expect("retry after fix -> ActionsRetried");
    let retried: ActionsRetried =
        extract_anchor_event(&retry_meta.logs).expect("ActionsRetried emitted");
    // Retry success is signaled by ActionsRetried, distinct from ActionsExecuted,
    // so the indexer can tell a first-run success from a retry success.
    assert!(extract_anchor_event::<ActionsExecuted>(&retry_meta.logs).is_none());

    // Reconciliation (events only): failure then resolution on the same fill.
    assert_eq!(failed.route, d.route);
    assert_eq!(failed.fill_id, d.fill_id);
    assert_eq!(
        retried.route, failed.route,
        "the retry resolves the failed fill"
    );
    assert_eq!(retried.fill_id, failed.fill_id);
    assert_eq!(retried.route, d.finalized.route);
    assert_eq!(retried.fill_id, d.finalized.fill_id);

    // Sanity (account state): funds delivered after the retry.
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &d.execution_vault), 0);
}

#[test]
fn event_stream_reconciles_finalize_then_recover() {
    let recipient = Pubkey::new_unique();
    let (mut svm, d) = drive_to_finalized(spl_config(recipient), [0x33; 32]);
    assert_verify_finalize_reconciles(&d);

    let owner_ata = create_ata(&mut svm, &d.payer, &d.owner.pubkey(), &d.mint);
    let meta = send_ix_with_meta(
        &mut svm,
        recover_funds_ix(&d.owner.pubkey(), &d.route, &d.fill_id, &d.mint, &owner_ata),
        &d.owner,
        &[],
    )
    .expect("recover straight from Finalized");
    let recovered: FundsRecovered =
        extract_anchor_event(&meta.logs).expect("FundsRecovered emitted");

    // Reconciliation (events only): recovery joins the finalized fill and the
    // recovered amount equals the finalized amount (full drain, observable in logs).
    assert_eq!(recovered.route, d.route);
    assert_eq!(recovered.fill_id, d.fill_id);
    assert_eq!(recovered.owner, d.owner.pubkey());
    assert_eq!(recovered.route, d.finalized.route);
    assert_eq!(recovered.fill_id, d.finalized.fill_id);
    assert_eq!(
        recovered.amount, d.finalized.amount,
        "recovered amount equals the finalized amount"
    );
    assert_eq!(recovered.amount, AMOUNT);

    // Sanity (account state): drained to the owner.
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &d.execution_vault), 0);
}
