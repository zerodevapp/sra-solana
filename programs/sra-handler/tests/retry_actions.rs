//! Owner-gated re-execution of a route's committed actions (`retry_actions`).
//! Re-runs the same `action_config` through the same shared engine as
//! `execute_actions` after a soft-fail, so an out-of-band fix settles on retry.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_instruction::AccountMeta;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use sra_common::{
    errors::SraError,
    quote::{action_context_hash, signed_quote_message},
    types::{CreateRouteParams, ExecutionStatus, ManageRouteParams, MinOut, SolanaAction},
};

const AMOUNT: u64 = 4_000_000;
const VAULT: u64 = 4_000_000;
const POOL_LIQUIDITY: u64 = 100_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_ID: [u8; 32] = [7u8; 32];
const DECIMALS: u8 = 6;

struct Staged {
    payer: Keypair,
    owner: Keypair,
    mint: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
}

/// Create a route, fund its execution vault, and seed a `FillReceipt` in
/// `initial_status`. Recipient ATAs are not created here (tests opt in).
fn stage(
    svm: &mut litesvm::LiteSVM,
    action_config: Vec<SolanaAction>,
    amount: u64,
    initial_status: ExecutionStatus,
) -> Staged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let salt = [3u8; 32];
    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: mint,
        action_config: action_config.clone(),
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
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
    let fill_receipt = seed_fill_receipt_with_status(svm, &route, FILL_ID, amount, initial_status);

    Staged {
        payer,
        owner,
        mint,
        route,
        execution_vault,
        fill_receipt,
    }
}

// --------------------------------------------------------------------------
// Success — a fix applied out-of-band settles on retry
// --------------------------------------------------------------------------

#[test]
fn retry_actions_after_fix_succeeds() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    // Finalized so the first execute_actions can run and soft-fail for real.
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::Finalized,
    );

    // First execution soft-fails: the recipient ATA does not exist yet.
    let missing = derive_ata(&recipient, &s.mint);
    let exec = execute_actions_ix(&s.payer.pubkey(), &s.route, FILL_ID, &s.mint, &[missing]);
    send_ix(&mut svm, exec, &s.payer, &[]).expect("missing ATA -> ActionsFailed");
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );

    // Fix: create the recipient ATA, then the owner retries.
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    let retry = retry_actions_ix(
        &s.owner.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, retry, &s.payer, &[&s.owner]).expect("retry after fix succeeds");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn retry_actions_limits_spending_to_fill_receipt_amount() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        1,
        ExecutionStatus::ActionsFailed,
    );
    let sibling_fill_id = [8u8; 32];
    let sibling_receipt = seed_fill_receipt_with_status(
        &mut svm,
        &s.route,
        sibling_fill_id,
        AMOUNT,
        ExecutionStatus::Finalized,
    );
    mint_to(
        &mut svm,
        &s.payer,
        &s.payer,
        &s.mint,
        &s.execution_vault,
        AMOUNT,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let retry = retry_actions_ix(
        &s.owner.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, retry, &s.payer, &[&s.owner]).expect("retry should succeed");

    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 1);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(status(&svm, &sibling_receipt), ExecutionStatus::Finalized);
}

#[test]
fn retry_actions_emits_actions_retried() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let retry = retry_actions_ix(
        &s.owner.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    let meta = send_ix_with_meta(&mut svm, retry, &s.payer, &[&s.owner]).expect("retry succeeds");
    let event: sra_handler::events::ActionsRetried =
        extract_anchor_event(&meta.logs).expect("ActionsRetried emitted");
    assert_eq!(event.route, s.route);
    assert_eq!(event.fill_id, FILL_ID);
}

#[test]
fn retry_actions_does_not_close_fill_receipt() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    let retry = retry_actions_ix(
        &s.owner.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    send_ix(&mut svm, retry, &s.payer, &[&s.owner]).expect("retry succeeds");

    // The replay marker must survive (closing it would re-enable replay of the fill).
    let acct = svm
        .get_account(&s.fill_receipt)
        .expect("fill_receipt still exists");
    assert!(!acct.data.is_empty(), "fill_receipt is not closed by retry");
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

// --------------------------------------------------------------------------
// Still failing — stays ActionsFailed, retryable indefinitely (no counter)
// --------------------------------------------------------------------------

#[test]
fn retry_actions_still_failing_stays_actions_failed() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    // The recipient ATA is still missing — the underlying problem is unfixed.
    let missing = derive_ata(&recipient, &s.mint);

    let retry = retry_actions_ix(&s.owner.pubkey(), &s.route, FILL_ID, &s.mint, &[missing]);
    // Returns Ok; the status stays ActionsFailed and can be retried again later.
    send_ix(&mut svm, retry, &s.payer, &[&s.owner])
        .expect("still-missing ATA -> stays ActionsFailed");

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

// --------------------------------------------------------------------------
// Hard rejections — status unchanged, funds preserved
// --------------------------------------------------------------------------

#[test]
fn retry_actions_non_actions_failed_status_rejected() {
    // Only ActionsFailed is retryable; every other state is rejected and untouched.
    for rejected in [
        ExecutionStatus::Finalized,
        ExecutionStatus::ActionsSucceeded,
        ExecutionStatus::Recovered,
    ] {
        let mut svm = setup_svm();
        let recipient = Pubkey::new_unique();
        let s = stage(
            &mut svm,
            vec![spl(recipient, 10_000)],
            AMOUNT,
            rejected.clone(),
        );
        // ATA exists, so the only possible rejection is the status gate itself.
        let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

        let retry = retry_actions_ix(
            &s.owner.pubkey(),
            &s.route,
            FILL_ID,
            &s.mint,
            &[recipient_ata],
        );
        let err = send_ix_expect_err(
            &mut svm,
            retry,
            &s.payer,
            &[&s.owner],
            "non-ActionsFailed receipt is not retryable",
        );
        assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));

        assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
        assert_eq!(status(&svm, &s.fill_receipt), rejected);
    }
}

#[test]
fn retry_actions_non_owner_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    let attacker = funded_keypair(&mut svm, 100 * SOL);

    // Attacker signs as the owner account; `has_one = owner` rejects the mismatch.
    let retry = retry_actions_ix(
        &attacker.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        retry,
        &s.payer,
        &[&attacker],
        "non-owner retry rejected",
    );
    assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));

    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn retry_actions_paused_route_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.mint);

    // Pause the route; retry is pause-gated.
    send_ix(
        &mut svm,
        manage_route_ix(
            &s.owner.pubkey(),
            &s.route,
            ManageRouteParams {
                pause: Some(true),
                adapters: None,
                executors: None,
            },
            &[],
        ),
        &s.payer,
        &[&s.owner],
    )
    .expect("pause should succeed");

    let retry = retry_actions_ix(
        &s.owner.pubkey(),
        &s.route,
        FILL_ID,
        &s.mint,
        &[recipient_ata],
    );
    let err = send_ix_expect_err(
        &mut svm,
        retry,
        &s.payer,
        &[&s.owner],
        "a paused route must reject retry_actions",
    );
    assert_eq!(err, custom_error(SraError::RoutePaused));

    // Funds + status untouched.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn retry_actions_spl_transfer_substitution_rejected() {
    let mut svm = setup_svm();
    let recipient = Pubkey::new_unique();
    let s = stage(
        &mut svm,
        vec![spl(recipient, 10_000)],
        AMOUNT,
        ExecutionStatus::ActionsFailed,
    );
    let _ = create_ata(&mut svm, &s.payer, &recipient, &s.mint);
    // A different recipient's ATA — correct shape, wrong pubkey. The exact-match
    // SplTransfer check must still bite on the retry path.
    let wrong_ata = derive_ata(&Pubkey::new_unique(), &s.mint);

    let retry = retry_actions_ix(&s.owner.pubkey(), &s.route, FILL_ID, &s.mint, &[wrong_ata]);
    let err = send_ix_expect_err(
        &mut svm,
        retry,
        &s.payer,
        &[&s.owner],
        "account substitution rejected on retry",
    );
    assert_eq!(err, custom_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

// --------------------------------------------------------------------------
// VerifiedCpi retry runs through the same shared engine as execute_actions
// --------------------------------------------------------------------------

/// One signed-quote VerifiedCpi action over the shared `mock_swap_template`.
fn verified_cpi(input_mint: Pubkey, output_mint: Pubkey, quoter: Pubkey) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps: 10_000,
        account_template: mock_swap_template(),
        output_mint,
        min_out: MinOut::SignedQuote { quoter },
        allow_permissioned_output: false,
    }
}

struct VerifiedStaged {
    payer: Keypair,
    owner: Keypair,
    input_mint: Pubkey,
    output_mint: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination: Pubkey,
    pool_authority: Pubkey,
    pool_input: Pubkey,
    pool_output: Pubkey,
}

impl VerifiedStaged {
    /// `[program, <10 template slots>]` for the single VerifiedCpi action (legacy
    /// output, so the output token program is the legacy SPL program).
    fn verified_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(self.destination, false),
            AccountMeta::new_readonly(self.output_mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(self.pool_output, false),
        ]
    }

    fn quote_message(&self, input_amount: u64, min_amount_out: u64, expiry: i64) -> Vec<u8> {
        let context_hash = action_context_hash(&self.route, &FILL_ID, 0);
        signed_quote_message(
            &context_hash,
            &self.input_mint,
            &self.output_mint,
            input_amount,
            min_amount_out,
            expiry,
        )
        .to_vec()
    }
}

/// Stage a route with one signed-quote VerifiedCpi action, a funded mock-swap
/// pool, scratch + destination ATAs, and an `ActionsFailed` receipt.
fn stage_verified(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> VerifiedStaged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let action_config = vec![verified_cpi(input_mint, output_mint, quoter.pubkey())];
    let salt = [4u8; 32];

    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: input_mint,
        action_config: action_config.clone(),
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
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
    let execution_vault = derive_ata(&execution_authority, &input_mint);
    mint_to(svm, &payer, &payer, &input_mint, &execution_vault, VAULT);

    let (scratch_authority, _) = derive_scratch_authority_pda(&route);
    let scratch = create_ata(svm, &payer, &scratch_authority, &input_mint);
    let destination = create_ata(svm, &payer, &owner.pubkey(), &output_mint);

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

    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, FILL_ID, VAULT, ExecutionStatus::ActionsFailed);

    VerifiedStaged {
        payer,
        owner,
        input_mint,
        output_mint,
        route,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        destination,
        pool_authority,
        pool_input,
        pool_output,
    }
}

#[test]
fn retry_actions_verified_cpi_reuses_engine() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let v = stage_verified(&mut svm, &quoter);

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &v.quote_message(VAULT, min_out, expiry));
    let retry = retry_actions_ix_full(
        &v.owner.pubkey(),
        &v.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out, expiry)],
        &v.input_mint,
        v.verified_metas(),
    );
    send_instructions(&mut svm, &[ed_ix, retry], &v.payer, &[&v.owner])
        .expect("VerifiedCpi retry settles through the shared engine");

    // The swap ran on the retry path: output delivered, scratch swept, vault spent.
    assert_eq!(fetch_token_balance(&svm, &v.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &v.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &v.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &v.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}
