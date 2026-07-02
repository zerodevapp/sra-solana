//! End-to-end destination-action lifecycle — the `ExecutionStatus` state machine.
//!
//! The per-instruction suites (`finalize_fill` / `execute_actions` /
//! `retry_actions` / `recover_funds` / `verified_cpi_actions`) each cover one
//! transition in isolation, and most start from a *planted* receipt. This suite
//! drives the **genuine `finalize_fill` entry** (relayer fill → adapter
//! `verify_across_fill` → `finalize_fill`) and then chains the *real*
//! `execute_actions` / `retry_actions` / `recover_funds` instructions over one
//! receipt — exercising the whole transition graph through real instructions and
//! proving the terminal states are truly closed.
//!
//! State machine:
//! ```text
//!   finalize_fill                          -> Finalized
//!   Finalized     --execute_actions-->        ActionsSucceeded (terminal) | ActionsFailed
//!   Finalized     --recover_funds(amt>0)-->   Recovered (terminal)
//!   ActionsFailed --retry_actions-->          ActionsSucceeded (terminal) | ActionsFailed (stays)
//!   ActionsFailed --recover_funds(amt>0)-->   Recovered (terminal)
//! ```
//!
//! Failure-model note: a `VerifiedCpi` `min_out` shortfall is a **hard tx revert**
//! that leaves the receipt `Finalized` (re-executable) — not a soft `ActionsFailed`
//! — because Solana cannot catch a CPI/post-condition failure mid-action (strictly
//! more conservative than EVM's catch). Only a recoverable per-account problem (a
//! missing/frozen recipient or destination ATA) is the soft `ActionsFailed` that
//! `retry_actions` fixes. `retry_actions` itself has no EVM analogue — EVM runs
//! actions inline in `onFill`; on Solana it is a native artifact of the decoupled
//! finalize→execute split, reusing the same (parity-verified) execution path.

mod common;

use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_sdk::{instruction::AccountMeta, pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_common::{
    errors::SraError,
    quote::{action_context_hash, signed_quote_message},
    types::{CreateRouteParams, ExecutionStatus, MinOut, SolanaAction},
};

const AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const POOL_LIQUIDITY: u64 = 100_000_000;
const DECIMALS: u8 = 6;
/// Matches `sample_relay_data`'s fill deadline (the relay hash is computed over it).
const FILL_DEADLINE: u32 = 1_900_000_000;

/// A route that has been driven through the real finalize path and is sitting in
/// `Finalized` with `AMOUNT` staged in its execution vault.
struct Finalized {
    route: Pubkey,
    fill_id: [u8; 32],
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
}

/// Create a route committing `action_config`, then drive the genuine finalize path
/// (relayer fill → adapter `verify_across_fill` → `finalize_fill`) so the receipt
/// lands in `Finalized` with `AMOUNT` of `mint` staged in the execution vault.
fn create_route_and_finalize(
    svm: &mut litesvm::LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    action_config: Vec<SolanaAction>,
    salt: [u8; 32],
) -> Finalized {
    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: mint,
        action_config,
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
        allowed_executors: vec![payer.pubkey()],
    };
    let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &salt);
    let create = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        route_config_hash,
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(svm, create, payer, &[owner]).expect("create_route should succeed");

    // A relayer fills the route vault exactly as the deployed Across program does,
    // the adapter records the verified fill, and the handler finalizes it.
    let route_vault = derive_ata(&route, &mint);
    let relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let relayer = funded_keypair(svm, 100 * SOL);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(svm, payer, payer, &mint, &relayer_ata, AMOUNT);
    send_ix(
        svm,
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
    send_ix(
        svm,
        verify_across_fill_ix(&payer.pubkey(), &route, relay_hash, relay_data),
        payer,
        &[],
    )
    .expect("verify_across_fill should succeed");
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    send_ix(
        svm,
        finalize_fill_ix(
            &payer.pubkey(),
            &route,
            route_config_hash,
            &verified_fill,
            relay_hash,
            &mint,
        ),
        payer,
        &[],
    )
    .expect("finalize_fill should succeed");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);
    let (fill_receipt, _) = derive_fill_receipt_pda(&route, &relay_hash);

    // Entry invariant: the full amount is staged and the receipt is Finalized.
    assert_eq!(fetch_token_balance(svm, &execution_vault), AMOUNT);
    assert_eq!(status(svm, &fill_receipt), ExecutionStatus::Finalized);

    Finalized {
        route,
        fill_id: relay_hash,
        execution_vault,
        fill_receipt,
    }
}

/// A single `SplTransfer` of 100% to `recipient`.
fn spl_config(recipient: Pubkey) -> Vec<SolanaAction> {
    vec![spl(recipient, 10_000)]
}

// ==========================================================================
// SplTransfer lifecycle paths
// ==========================================================================

#[test]
fn lifecycle_finalize_execute_fail_retry_fail_recover() {
    // Finalized -> ActionsFailed -> (retry) ActionsFailed -> (recover) Recovered.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let cfg = spl_config(recipient);
    let f = create_route_and_finalize(&mut svm, &payer, &owner, mint, cfg.clone(), [0x01; 32]);

    // execute_actions with the recipient ATA missing -> soft fail (no transfer).
    let missing = derive_ata(&recipient, &mint);
    send_ix(
        &mut svm,
        execute_actions_ix(&payer.pubkey(), &f.route, f.fill_id, &mint, &[missing]),
        &payer,
        &[],
    )
    .expect("missing recipient ATA -> ActionsFailed (soft, returns Ok)");
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);

    // An ActionsFailed receipt cannot re-enter via execute_actions (which gates on
    // Finalized); the only forward op is retry_actions (or recover_funds).
    let reexec = send_ix_expect_err(
        &mut svm,
        execute_actions_ix(&payer.pubkey(), &f.route, f.fill_id, &mint, &[missing]),
        &payer,
        &[],
        "execute_actions on an ActionsFailed receipt must reject",
    );
    assert_eq!(reexec, custom_error(SraError::InvalidExecutionStatus));
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsFailed
    );

    // retry while the ATA is still missing -> stays ActionsFailed (retryable).
    send_ix(
        &mut svm,
        retry_actions_ix(&owner.pubkey(), &f.route, f.fill_id, &mint, &[missing]),
        &payer,
        &[&owner],
    )
    .expect("still-missing ATA -> stays ActionsFailed");
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);

    // recover_funds drains the staged amount to the owner and lands at Recovered.
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &f.route, &f.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover from ActionsFailed -> Recovered");
    assert_eq!(status(&svm, &f.fill_receipt), ExecutionStatus::Recovered);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT);
}

#[test]
fn lifecycle_finalize_execute_succeed_terminal() {
    // Finalized -> ActionsSucceeded (terminal closure proven in
    // lifecycle_terminal_states_reject_further_ops).
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let cfg = spl_config(recipient);
    let f = create_route_and_finalize(&mut svm, &payer, &owner, mint, cfg.clone(), [0x02; 32]);

    let recipient_ata = create_ata(&mut svm, &payer, &recipient, &mint);
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &f.route,
            f.fill_id,
            &mint,
            &[recipient_ata],
        ),
        &payer,
        &[],
    )
    .expect("execute_actions should succeed");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), 0);
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn lifecycle_finalize_execute_fail_retry_succeed_terminal() {
    // Finalized -> ActionsFailed -> (fix ATA, retry) ActionsSucceeded.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let cfg = spl_config(recipient);
    let f = create_route_and_finalize(&mut svm, &payer, &owner, mint, cfg.clone(), [0x03; 32]);

    let missing = derive_ata(&recipient, &mint);
    send_ix(
        &mut svm,
        execute_actions_ix(&payer.pubkey(), &f.route, f.fill_id, &mint, &[missing]),
        &payer,
        &[],
    )
    .expect("missing recipient ATA -> ActionsFailed");
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsFailed
    );

    // Fix the out-of-band problem, then the owner retries.
    let recipient_ata = create_ata(&mut svm, &payer, &recipient, &mint);
    send_ix(
        &mut svm,
        retry_actions_ix(
            &owner.pubkey(),
            &f.route,
            f.fill_id,
            &mint,
            &[recipient_ata],
        ),
        &payer,
        &[&owner],
    )
    .expect("retry after fix -> ActionsSucceeded");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), 0);
    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn lifecycle_finalize_recover_skip_actions_terminal() {
    // Finalized -> Recovered directly (actions never run; e.g. a route the owner
    // chooses to unwind instead of executing).
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let f = create_route_and_finalize(
        &mut svm,
        &payer,
        &owner,
        mint,
        spl_config(recipient),
        [0x04; 32],
    );

    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &f.route, &f.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover straight from Finalized -> Recovered");

    assert_eq!(status(&svm, &f.fill_receipt), ExecutionStatus::Recovered);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT);
}

/// Every lifecycle-mutating op rejects a terminal receipt with
/// `InvalidExecutionStatus`, leaving status and funds untouched.
fn assert_all_ops_rejected(
    svm: &mut litesvm::LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    f: &Finalized,
    mint: Pubkey,
    atas: (Pubkey, Pubkey),
) {
    let (recipient_ata, owner_ata) = atas;
    let before = status(svm, &f.fill_receipt);
    let vault_before = fetch_token_balance(svm, &f.execution_vault);

    // Status gate fires before the executor/pause gates.
    let err = send_ix_expect_err(
        svm,
        execute_actions_ix(
            &payer.pubkey(),
            &f.route,
            f.fill_id,
            &mint,
            &[recipient_ata],
        ),
        payer,
        &[],
        "execute on a terminal receipt must reject",
    );
    assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));

    let err = send_ix_expect_err(
        svm,
        retry_actions_ix(
            &owner.pubkey(),
            &f.route,
            f.fill_id,
            &mint,
            &[recipient_ata],
        ),
        payer,
        &[owner],
        "retry on a terminal receipt must reject",
    );
    assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));

    let err = send_ix_expect_err(
        svm,
        recover_funds_ix(&owner.pubkey(), &f.route, &f.fill_id, &mint, &owner_ata),
        owner,
        &[],
        "recover on a terminal receipt must reject",
    );
    assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));

    assert_eq!(
        status(svm, &f.fill_receipt),
        before,
        "status must be unchanged"
    );
    assert_eq!(
        fetch_token_balance(svm, &f.execution_vault),
        vault_before,
        "no funds may move"
    );
}

#[test]
fn lifecycle_terminal_states_reject_further_ops() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);

    // ----- first terminal state: ActionsSucceeded -----
    let recipient_a = Pubkey::new_unique();
    let cfg_a = spl_config(recipient_a);
    let fa = create_route_and_finalize(&mut svm, &payer, &owner, mint, cfg_a.clone(), [0x05; 32]);
    let recipient_a_ata = create_ata(&mut svm, &payer, &recipient_a, &mint);
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fa.route,
            fa.fill_id,
            &mint,
            &[recipient_a_ata],
        ),
        &payer,
        &[],
    )
    .expect("reach ActionsSucceeded");
    assert_eq!(
        status(&svm, &fa.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_all_ops_rejected(
        &mut svm,
        &payer,
        &owner,
        &fa,
        mint,
        (recipient_a_ata, owner_ata),
    );

    // ----- second terminal state: Recovered -----
    let recipient_b = Pubkey::new_unique();
    let cfg_b = spl_config(recipient_b);
    let fb = create_route_and_finalize(&mut svm, &payer, &owner, mint, cfg_b.clone(), [0x06; 32]);
    let recipient_b_ata = create_ata(&mut svm, &payer, &recipient_b, &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &fb.route, &fb.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("reach Recovered");
    assert_eq!(status(&svm, &fb.fill_receipt), ExecutionStatus::Recovered);
    assert_all_ops_rejected(
        &mut svm,
        &payer,
        &owner,
        &fb,
        mint,
        (recipient_b_ata, owner_ata),
    );
}

#[test]
fn lifecycle_empty_action_config_actions_succeeded_immediately() {
    // An empty action_config is a valid (if degenerate) commitment: execute_actions
    // runs no transfers and marks ActionsSucceeded. The staged funds simply stay in
    // the execution vault (the owner committed to doing nothing with them).
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let f = create_route_and_finalize(&mut svm, &payer, &owner, mint, vec![], [0x07; 32]);

    send_ix(
        &mut svm,
        execute_actions_ix(&payer.pubkey(), &f.route, f.fill_id, &mint, &[]),
        &payer,
        &[],
    )
    .expect("empty action config -> ActionsSucceeded immediately");

    assert_eq!(
        status(&svm, &f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);

    // The receipt is now terminal, so the still-staged funds are NOT recoverable:
    // recover_funds rejects on the status gate, not the empty-vault gate. An empty
    // action_config is therefore a self-inflicted stranding the owner commits to at
    // create-route time. (The recipient ATA slot is irrelevant here — execute/retry
    // reject on the status gate before touching remaining accounts.)
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    assert_all_ops_rejected(&mut svm, &payer, &owner, &f, mint, (owner_ata, owner_ata));
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);
}

// ==========================================================================
// VerifiedCpi lifecycle paths (signed-quote swap through the shared engine)
// ==========================================================================

/// One signed-quote `VerifiedCpi` action over the shared `mock_swap` template.
fn verified_cpi(
    input_mint: Pubkey,
    output_mint: Pubkey,
    input_bps: u16,
    quoter: Pubkey,
) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps,
        account_template: mock_swap_template(),
        output_mint,
        min_out: MinOut::SignedQuote { quoter },
        allow_permissioned_output: false,
    }
}

struct VerifiedSetup {
    payer: Keypair,
    owner: Keypair,
    input_mint: Pubkey,
    output_mint: Pubkey,
    f: Finalized,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination: Pubkey,
    pool_authority: Pubkey,
    pool_input: Pubkey,
    pool_output: Pubkey,
}

impl VerifiedSetup {
    /// `[program, <10 template slots>]` for the single VerifiedCpi action (legacy
    /// output, so the output token program is the legacy SPL program). `destination`
    /// is overridable for redirect tests, though lifecycle paths pass the pinned one.
    fn verified_metas(&self, destination: Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(self.output_mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(self.pool_output, false),
        ]
    }

    fn quote_message(
        &self,
        action_index: u8,
        input_amount: u64,
        min_out: u64,
        expiry: i64,
    ) -> Vec<u8> {
        let context_hash = action_context_hash(&self.f.route, &self.f.fill_id, action_index);
        signed_quote_message(
            &context_hash,
            &self.input_mint,
            &self.output_mint,
            input_amount,
            min_out,
            expiry,
        )
        .to_vec()
    }
}

/// Stage a route whose committed action is one 100% signed-quote `VerifiedCpi`,
/// finalized through the genuine path, with the scratch ATA + a funded mock-swap
/// pool ready. The owner's output destination ATA is created iff `create_destination`.
fn stage_verified(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    create_destination: bool,
    salt: [u8; 32],
) -> VerifiedSetup {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let action_config = vec![verified_cpi(
        input_mint,
        output_mint,
        10_000,
        quoter.pubkey(),
    )];

    let f = create_route_and_finalize(svm, &payer, &owner, input_mint, action_config.clone(), salt);

    let (scratch_authority, _) = derive_scratch_authority_pda(&f.route);
    let scratch = create_ata(svm, &payer, &scratch_authority, &input_mint);
    let destination = if create_destination {
        create_ata(svm, &payer, &owner.pubkey(), &output_mint)
    } else {
        derive_ata(&owner.pubkey(), &output_mint)
    };

    let (pool_authority, pool_input, pool_output) =
        derive_mock_swap_pool(&input_mint, &output_mint);
    create_ata(svm, &payer, &pool_authority, &input_mint);
    create_ata(svm, &payer, &pool_authority, &output_mint);
    mint_to(
        svm,
        &payer,
        &payer,
        &output_mint,
        &pool_output,
        POOL_LIQUIDITY,
    );

    VerifiedSetup {
        payer,
        owner,
        input_mint,
        output_mint,
        f,
        scratch_authority,
        scratch,
        destination,
        pool_authority,
        pool_input,
        pool_output,
    }
}

#[test]
fn lifecycle_finalize_verified_cpi_swap_succeed_terminal() {
    // Finalized -> (VerifiedCpi swap) ActionsSucceeded.
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let v = stage_verified(&mut svm, &quoter, true, [0x08; 32]);

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &v.quote_message(0, AMOUNT, min_out, expiry));
    let exec = execute_actions_ix_full(
        &v.payer.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(swap_data(AMOUNT, out_amount), min_out, expiry)],
        &v.input_mint,
        v.verified_metas(v.destination),
    );
    send_instructions(&mut svm, &[ed_ix, exec], &v.payer, &[]).expect("verified swap succeeds");

    assert_eq!(fetch_token_balance(&svm, &v.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &v.f.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &v.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &v.f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn lifecycle_verified_cpi_below_min_out_reverts_then_reexecutes() {
    // A below-min_out swap HARD-reverts (MinOutNotMet) and leaves the receipt
    // Finalized — re-executable. A corrected quote on a second execute_actions then
    // settles it. (This is NOT a retry_actions path: the receipt never went to
    // ActionsFailed.)
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let v = stage_verified(&mut svm, &quoter, true, [0x09; 32]);
    let expiry = now(&svm) + 3_600;

    // Quote demands 3_000_000 but the swap only delivers 2_000_000 -> hard revert.
    let ed_short = ed25519_quote_ix(&quoter, &v.quote_message(0, AMOUNT, 3_000_000, expiry));
    let exec_short = execute_actions_ix_full(
        &v.payer.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(swap_data(AMOUNT, 2_000_000), 3_000_000, expiry)],
        &v.input_mint,
        v.verified_metas(v.destination),
    );
    let err = send_instructions_expect_err(&mut svm, &[ed_short, exec_short], &v.payer, &[]);
    // execute_actions sits at index 1 (the ed25519 quote is index 0).
    assert_eq!(
        err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            solana_sdk::instruction::InstructionError::Custom(SraError::MinOutNotMet.into()),
        )
    );
    assert_eq!(fetch_token_balance(&svm, &v.f.execution_vault), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &v.destination), 0);
    assert_eq!(status(&svm, &v.f.fill_receipt), ExecutionStatus::Finalized);

    // Re-execute with a quote the swap can satisfy.
    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let ed_ok = ed25519_quote_ix(&quoter, &v.quote_message(0, AMOUNT, min_out, expiry));
    let exec_ok = execute_actions_ix_full(
        &v.payer.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(swap_data(AMOUNT, out_amount), min_out, expiry)],
        &v.input_mint,
        v.verified_metas(v.destination),
    );
    send_instructions(&mut svm, &[ed_ok, exec_ok], &v.payer, &[]).expect("re-execution settles");

    assert_eq!(fetch_token_balance(&svm, &v.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &v.f.execution_vault), 0);
    assert_eq!(
        status(&svm, &v.f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn lifecycle_verified_cpi_missing_destination_soft_fail_retry_succeed() {
    // The genuine VerifiedCpi soft-fail path: a missing destination ATA is a
    // recoverable per-account problem -> ActionsFailed; create it, retry -> succeed.
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let v = stage_verified(&mut svm, &quoter, false, [0x0A; 32]);
    let expiry = now(&svm) + 3_600;

    let ed_ix = ed25519_quote_ix(&quoter, &v.quote_message(0, AMOUNT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &v.payer.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(swap_data(AMOUNT, 1_000_000), 1_000_000, expiry)],
        &v.input_mint,
        v.verified_metas(v.destination),
    );
    send_instructions(&mut svm, &[ed_ix, exec], &v.payer, &[])
        .expect("missing destination ATA -> ActionsFailed (soft)");
    assert_eq!(
        status(&svm, &v.f.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    assert_eq!(fetch_token_balance(&svm, &v.f.execution_vault), AMOUNT);

    // Fix the destination ATA out-of-band, then the owner retries.
    create_ata(&mut svm, &v.payer, &v.owner.pubkey(), &v.output_mint);
    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry2 = now(&svm) + 3_600;
    let ed_retry = ed25519_quote_ix(&quoter, &v.quote_message(0, AMOUNT, min_out, expiry2));
    let retry = retry_actions_ix_full(
        &v.owner.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(swap_data(AMOUNT, out_amount), min_out, expiry2)],
        &v.input_mint,
        v.verified_metas(v.destination),
    );
    send_instructions(&mut svm, &[ed_retry, retry], &v.payer, &[&v.owner])
        .expect("retry after creating the destination ATA -> ActionsSucceeded");

    assert_eq!(fetch_token_balance(&svm, &v.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &v.f.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &v.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &v.f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn lifecycle_mixed_spl_transfer_and_verified_cpi_in_one_route() {
    // One route, one action_config holding both variants: 30% straight transfer +
    // 70% swapped, both off the start-of-run snapshot, finalized then executed.
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let input_mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let action_config = vec![
        spl(recipient, 3_000),
        verified_cpi(input_mint, output_mint, 7_000, quoter.pubkey()),
    ];

    let f = create_route_and_finalize(
        &mut svm,
        &payer,
        &owner,
        input_mint,
        action_config.clone(),
        [0x0B; 32],
    );

    // Wire the same swap infra stage_verified builds, but for this mixed route.
    let (scratch_authority, _) = derive_scratch_authority_pda(&f.route);
    let scratch = create_ata(&mut svm, &payer, &scratch_authority, &input_mint);
    let destination = create_ata(&mut svm, &payer, &owner.pubkey(), &output_mint);
    let (pool_authority, pool_input, pool_output) =
        derive_mock_swap_pool(&input_mint, &output_mint);
    create_ata(&mut svm, &payer, &pool_authority, &input_mint);
    create_ata(&mut svm, &payer, &pool_authority, &output_mint);
    mint_to(
        &mut svm,
        &payer,
        &payer,
        &output_mint,
        &pool_output,
        POOL_LIQUIDITY,
    );
    let recipient_ata = create_ata(&mut svm, &payer, &recipient, &input_mint);

    let v = VerifiedSetup {
        payer,
        owner,
        input_mint,
        output_mint,
        f,
        scratch_authority,
        scratch,
        destination,
        pool_authority,
        pool_input,
        pool_output,
    };

    let swap_input = AMOUNT * 7_000 / 10_000;
    let out_amount = 1_500_000;
    let expiry = now(&svm) + 3_600;
    // The VerifiedCpi is action index 1, so the quote binds to that index.
    let ed_ix = ed25519_quote_ix(&quoter, &v.quote_message(1, swap_input, out_amount, expiry));
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(v.verified_metas(v.destination));
    let exec = execute_actions_ix_full(
        &v.payer.pubkey(),
        &v.f.route,
        v.f.fill_id,
        vec![runtime(
            swap_data(swap_input, out_amount),
            out_amount,
            expiry,
        )],
        &v.input_mint,
        remaining,
    );
    send_instructions(&mut svm, &[ed_ix, exec], &v.payer, &[]).expect("mixed config composes");

    assert_eq!(
        fetch_token_balance(&svm, &recipient_ata),
        AMOUNT * 3_000 / 10_000
    );
    assert_eq!(fetch_token_balance(&svm, &v.destination), out_amount);
    assert_eq!(
        fetch_token_balance(&svm, &v.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &v.f.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}
