//! Destination action execution (`execute_actions`).
//!
//! Runs a finalized route's committed `action_config` against the funds staged
//! in its execution vault. This is the Solana analog of the EVM
//! `_resolveAndExecuteActions` — the action-running half of `onFill` (the
//! settlement half is `finalize_fill`). v1 executes the static `SplTransfer`
//! variant; the dynamic `VerifiedCpi` variant fails closed until its engine
//! ships.
//!
//! Failure model (EVM `try/catch` + atomicity parity): a bad recipient-ATA
//! runtime state (missing / frozen) sets `ActionsFailed` and persists it with no
//! partial transfers (validate-then-execute two-pass); a structural relayer
//! error (wrong/missing accounts) hard-rejects, leaving the receipt `Finalized`;
//! CU exhaustion aborts atomically, also leaving it `Finalized`.

mod common;

use common::*;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, pubkey::Pubkey, signature::Keypair, signer::Signer,
};
use sra_common::{
    errors::SraError,
    types::{
        AccountTemplate, CreateRouteParams, ExecutionStatus, FixedRole, ManageRouteParams, MinOut,
        SlotRole, SolanaAction,
    },
};

const AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_ID: [u8; 32] = [7u8; 32];

/// A valid `VerifiedCpi` action (pins all four required slots, non-zero keys) —
/// creatable at `create_route`, not executable in this build.
fn verified_cpi() -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: Pubkey::new_unique(),
        input_mint: Pubkey::new_unique(),
        input_bps: 10_000,
        account_template: AccountTemplate {
            slots: vec![
                SlotRole::Fixed(FixedRole::TokenProgram),
                SlotRole::Fixed(FixedRole::SigningAuthority),
                SlotRole::Fixed(FixedRole::Source),
                SlotRole::Fixed(FixedRole::Destination),
                SlotRole::Fixed(FixedRole::OutputMint),
                SlotRole::Free,
            ],
        },
        output_mint: Pubkey::new_unique(),
        min_out: MinOut::Oracle {
            feed: Pubkey::new_unique(),
            max_slippage_bps: 50,
        },
        allow_permissioned_output: false,
    }
}

struct Staged {
    payer: solana_sdk::signature::Keypair,
    owner: solana_sdk::signature::Keypair,
    mint: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    action_config: Vec<SolanaAction>,
}

/// Create a route with `action_config`, fund its execution vault with `amount`,
/// and seed a `Finalized` `FillReceipt` — leaving the route ready for
/// `execute_actions`. Recipient ATAs are NOT created here (tests opt in, so the
/// missing-ATA paths are reachable).
fn stage(svm: &mut litesvm::LiteSVM, action_config: Vec<SolanaAction>, amount: u64) -> Staged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let salt = [1u8; 32];
    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: mint,
        action_config: action_config.clone(),
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
        // Fee payer is the authorized executor; gate tests below use distinct keypairs.
        allowed_executors: vec![payer.pubkey()],
    };
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &hash, &salt);
    let ix = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        hash,
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(svm, ix, &payer, &[&owner]).expect("create_route should succeed");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);
    if amount > 0 {
        mint_to(svm, &payer, &payer, &mint, &execution_vault, amount);
    }
    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, FILL_ID, amount, ExecutionStatus::Finalized);

    Staged {
        payer,
        owner,
        mint,
        route,
        execution_vault,
        fill_receipt,
        action_config,
    }
}

// --------------------------------------------------------------------------
// Success paths
// --------------------------------------------------------------------------

#[test]
fn execute_actions_single_spl_transfer_succeeds() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("execute_actions should succeed");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn execute_actions_multiple_all_succeed() {
    let mut svm = setup_svm();
    let r1 = Pubkey::new_unique();
    let r2 = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(r1, 6_000), spl(r2, 4_000)], AMOUNT);
    let a1 = create_ata(&mut svm, &s.payer, &r1, &s.mint);
    let a2 = create_ata(&mut svm, &s.payer, &r2, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[a1, a2],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("execute_actions should succeed");

    // 60/40 of the same start-of-run snapshot drains the vault exactly.
    assert_eq!(fetch_token_balance(&svm, &a1), 2_400_000);
    assert_eq!(fetch_token_balance(&svm, &a2), 1_600_000);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn execute_actions_empty_action_config_immediately_succeeds() {
    let mut svm = setup_svm();
    let s = stage(&mut svm, vec![], AMOUNT);

    let ix = execute_actions_ix(&s.payer.pubkey(), &s.route, FILL_ID, vec![], &s.mint, &[]);
    send_ix(&mut svm, ix, &s.payer, &[]).expect("empty action_config is a no-op success");

    // No actions, no transfers — funds stay staged, status is terminal-success.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn execute_actions_partial_bps_sum_leaves_dust_and_succeeds() {
    let mut svm = setup_svm();
    let r1 = Pubkey::new_unique();
    let r2 = Pubkey::new_unique();
    // 60% + 30% = 90% of the vault; the remaining 10% is left staged (recoverable
    // later). Σ amount_bps < BPS_DENOMINATOR is a valid config, not a failure.
    let s = stage(&mut svm, vec![spl(r1, 6_000), spl(r2, 3_000)], AMOUNT);
    let a1 = create_ata(&mut svm, &s.payer, &r1, &s.mint);
    let a2 = create_ata(&mut svm, &s.payer, &r2, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[a1, a2],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("under-100% drain succeeds");

    assert_eq!(fetch_token_balance(&svm, &a1), 2_400_000);
    assert_eq!(fetch_token_balance(&svm, &a2), 1_200_000);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 400_000);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn execute_actions_emits_actions_executed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let meta = send_ix_with_meta(&mut svm, ix, &s.payer, &[]).expect("execute_actions");
    let event: sra_handler::events::ActionsExecuted =
        extract_anchor_event(&meta.logs).expect("ActionsExecuted emitted");
    assert_eq!(event.route, s.route);
    assert_eq!(event.fill_id, FILL_ID);
}

// --------------------------------------------------------------------------
// Soft failure (ActionsFailed) — funds preserved, status persisted
// --------------------------------------------------------------------------

#[test]
fn execute_actions_failure_sets_actions_failed_and_preserves_funds() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    // Recipient ATA intentionally NOT created.
    let recipient_ata = derive_ata(&recipient, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    // Returns Ok — the ActionsFailed status write must persist.
    send_ix(&mut svm, ix, &s.payer, &[])
        .expect("missing recipient ATA -> ActionsFailed, not abort");

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    assert!(
        svm.get_account(&recipient_ata)
            .is_none_or(|a| a.data.is_empty()),
        "recipient ATA was never created"
    );
}

#[test]
fn execute_actions_frozen_recipient_ata_sets_actions_failed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    freeze_token_account(&mut svm, &recipient_ata);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("frozen recipient ATA -> ActionsFailed");

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn execute_actions_frozen_source_vault_sets_actions_failed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    // Freeze the SOURCE execution vault (a freeze-authority mint can freeze any
    // account of its mint, including this PDA-owned vault). The transfer would
    // abort, so the engine must preflight it into a retryable ActionsFailed
    // rather than hard-aborting and stranding the receipt at Finalized.
    freeze_token_account(&mut svm, &s.execution_vault);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("frozen source vault -> ActionsFailed, not abort");

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn execute_actions_partial_failure_atomic() {
    let mut svm = setup_svm();
    let r1 = Pubkey::new_unique();
    let r2 = Pubkey::new_unique();
    let r3 = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(r1, 3_000), spl(r2, 3_000), spl(r3, 4_000)],
        AMOUNT,
    );
    let a1 = create_ata(&mut svm, &s.payer, &r1, &s.mint);
    let a2 = derive_ata(&r2, &s.mint); // the middle action's ATA is missing
    let a3 = create_ata(&mut svm, &s.payer, &r3, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[a1, a2, a3],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("returns Ok with ActionsFailed");

    // The whole batch unwound — NO partial transfers to actions 1 or 3.
    assert_eq!(fetch_token_balance(&svm, &a1), 0);
    assert_eq!(fetch_token_balance(&svm, &a3), 0);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn execute_actions_emits_actions_failed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = derive_ata(&recipient, &s.mint); // not created

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let meta =
        send_ix_with_meta(&mut svm, ix, &s.payer, &[]).expect("ActionsFailed path returns Ok");
    let event: sra_handler::events::ActionsFailed =
        extract_anchor_event(&meta.logs).expect("ActionsFailed emitted");
    assert_eq!(event.route, s.route);
    assert_eq!(event.fill_id, FILL_ID);
}

// --------------------------------------------------------------------------
// Hard rejections — receipt stays Finalized, retryable
// --------------------------------------------------------------------------

#[test]
fn execute_actions_non_finalized_status_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, ix, &s.payer, &[]).expect("first execute succeeds");
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );

    // A second execute on an already-succeeded receipt is rejected.
    let ix2 = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(&mut svm, ix2, &s.payer, &[], "non-finalized rejected");
    assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));
}

#[test]
fn execute_actions_action_config_must_hash_match_committed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // Same recipient, different amount_bps -> different action_config_hash.
    let tampered = vec![spl(recipient, 5_000)];
    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        tampered,
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[],
        "tampered action_config rejected",
    );
    assert_eq!(err, custom_error(SraError::ActionConfigHashMismatch));

    // Funds untouched, receipt still finalizable.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn execute_actions_spl_transfer_remaining_accounts_substitution_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let _ = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    // A different recipient's ATA — correct shape, wrong pubkey.
    let wrong_ata = derive_ata(&Pubkey::new_unique(), &s.mint);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[wrong_ata],
    );
    let err = send_ix_expect_err(&mut svm, ix, &s.payer, &[], "account substitution rejected");
    assert_eq!(err, custom_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn execute_actions_spl_transfer_non_writable_recipient_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let mut ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    // Flip the recipient ATA (the only remaining account) to read-only.
    let last = ix.accounts.len() - 1;
    ix.accounts[last].is_writable = false;

    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[],
        "non-writable recipient rejected",
    );
    assert_eq!(err, custom_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
}

#[test]
fn execute_actions_verified_cpi_action_unsupported() {
    let mut svm = setup_svm();
    let s = stage(&mut svm, vec![verified_cpi()], AMOUNT);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[],
        "VerifiedCpi not executable yet",
    );
    assert_eq!(err, custom_error(SraError::UnsupportedAction));

    // Fail-closed: funds stay, receipt stays Finalized (re-executable once the
    // VerifiedCpi engine ships — not a terminal ActionsFailed).
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn execute_actions_cu_exhaustion_leaves_receipt_retryable() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // A CU limit far below what execute_actions needs aborts the whole tx.
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000);
    let exec_ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let res = send_instructions(&mut svm, &[cu_ix, exec_ix], &s.payer, &[]);
    assert!(res.is_err(), "CU exhaustion must abort the transaction");

    // Atomic abort — nothing half-applied.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);

    // Retry with a normal budget succeeds.
    let retry = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, retry, &s.payer, &[]).expect("retry after CU exhaustion succeeds");
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

// --------------------------------------------------------------------------
// Executor allowlist + pause gates
// --------------------------------------------------------------------------

/// Set the route's executor allowlist via manage_route.
fn set_executors(svm: &mut litesvm::LiteSVM, s: &Staged, executors: Vec<Pubkey>) {
    send_ix(
        svm,
        manage_route_ix(
            &s.owner.pubkey(),
            &s.route,
            ManageRouteParams {
                pause: None,
                adapters: None,
                executors: Some(executors),
            },
            &[],
        ),
        &s.payer,
        &[&s.owner],
    )
    .expect("manage_route (executors) should succeed");
}

fn set_paused(svm: &mut litesvm::LiteSVM, s: &Staged, paused: bool) {
    send_ix(
        svm,
        manage_route_ix(
            &s.owner.pubkey(),
            &s.route,
            ManageRouteParams {
                pause: Some(paused),
                adapters: None,
                executors: None,
            },
            &[],
        ),
        &s.payer,
        &[&s.owner],
    )
    .expect("manage_route (pause) should succeed");
}

#[test]
fn execute_actions_authorized_distinct_executor_succeeds() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // A curated executor distinct from payer/owner.
    let executor = Keypair::new();
    set_executors(&mut svm, &s, vec![executor.pubkey()]);

    let ix = execute_actions_ix(
        &executor.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, ix, &s.payer, &[&executor])
        .expect("a curated executor can run execute_actions");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn execute_actions_unauthorized_caller_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // A signer not in allowed_executors (route authorizes only the payer).
    let intruder = Keypair::new();
    let ix = execute_actions_ix(
        &intruder.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[&intruder],
        "an unauthorized executor must be rejected",
    );
    assert_eq!(err, custom_error(SraError::ExecutorNotAllowed));

    // Funds untouched; receipt stays Finalized.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn execute_actions_empty_executors_freezes() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // Owner clears the allowlist (freezes execution).
    set_executors(&mut svm, &s, vec![]);

    // Even the payer is now rejected.
    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[],
        "an empty executor allowlist freezes execute_actions",
    );
    assert_eq!(err, custom_error(SraError::ExecutorNotAllowed));

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn execute_actions_paused_route_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(&mut svm, vec![spl(recipient, 10_000)], AMOUNT);
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // Paused; payer is authorized, so the pause gate (checked first) rejects.
    set_paused(&mut svm, &s, true);

    let ix = execute_actions_ix(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        ix,
        &s.payer,
        &[],
        "a paused route must reject execute_actions",
    );
    assert_eq!(err, custom_error(SraError::RoutePaused));

    // Funds + receipt untouched.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}
