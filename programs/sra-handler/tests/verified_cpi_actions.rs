//! Verified-CPI action execution (the shared engine + co-signing quoter path +
//! Token-2022 output handling).
//!
//! Exercises the engine's security model against a generic balance-diff swap mock:
//! scratch-bounded input, pinned source/destination/authority, balance-diff
//! `≥ min_out` from a route-committed co-signing quoter, fail-closed, scratch swept
//! empty. The output may be a Token-2022 mint: the balance-diff is post-fee, and a
//! permissioned (permanent-delegate / pausable / default-frozen) output requires an
//! explicit opt-in. The concrete Jupiter template is a follow-up; here the swap is a
//! mock and every `min_amount_out` (plus the batch `quote_expiry`) is attested by the
//! one quoter that co-signs the transaction.

mod common;

use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::TransactionError,
};
use sra_common::{
    errors::SraError,
    types::{CreateRouteParams, ExecutionStatus, SolanaAction},
};

const VAULT: u64 = 4_000_000;
const POOL_LIQUIDITY: u64 = 100_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_ID: [u8; 32] = [9u8; 32];
const DECIMALS: u8 = 6;

fn custom_error_at(index: u8, err: SraError) -> TransactionError {
    TransactionError::InstructionError(index, InstructionError::Custom(err.into()))
}

/// The execute_actions instruction sits alone (index 0) once the ed25519 quote
/// instruction is gone — the quoter co-signs the transaction instead.
fn exec_error(err: SraError) -> TransactionError {
    custom_error_at(0, err)
}

/// How the staged route's output mint is created — drives its token program and
/// any Token-2022 extensions.
#[derive(Clone, Copy)]
enum OutKind {
    Legacy,
    Token2022,
    Token2022Fee { fee_bps: u16, max_fee: u64 },
    Token2022Permissioned,
}

impl OutKind {
    fn token_program(self) -> Pubkey {
        match self {
            OutKind::Legacy => TOKEN_PROGRAM_ID,
            _ => TOKEN_2022_PROGRAM_ID,
        }
    }
}

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
        quoter,
        allow_permissioned_output: false,
    }
}

struct Staged {
    payer: Keypair,
    input_mint: Pubkey,
    output_mint: Pubkey,
    output_token_program: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination: Pubkey,
    pool_input: Pubkey,
    pool_output: Pubkey,
    pool_authority: Pubkey,
}

/// Create the input (legacy) + output mints, build `action_config` from them (and
/// `quoter`), create the route, fund the execution vault, create + fund the mock
/// pool, create the scratch ATA, and seed a `Finalized` receipt. The output mint's
/// program/extensions follow `out_kind`; the destination ATA is created iff
/// `create_destination`.
fn stage(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    out_kind: OutKind,
    make_actions: impl FnOnce(Pubkey, Pubkey, Pubkey) -> Vec<SolanaAction>,
    create_destination: bool,
) -> Staged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    // The bridged input is always legacy SPL (`expected_mint`).
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_token_program = out_kind.token_program();
    let output_mint = match out_kind {
        OutKind::Legacy => create_mint(svm, &payer, &payer.pubkey(), DECIMALS),
        OutKind::Token2022 => create_mint_2022(svm, &payer, &payer.pubkey(), DECIMALS),
        OutKind::Token2022Fee { fee_bps, max_fee } => {
            create_transfer_fee_mint_2022(svm, &payer, &payer.pubkey(), DECIMALS, fee_bps, max_fee)
        }
        OutKind::Token2022Permissioned => {
            create_permissioned_mint_2022(svm, &payer, &payer.pubkey(), DECIMALS)
        }
    };
    let action_config = make_actions(input_mint, output_mint, quoter.pubkey());
    let salt = [2u8; 32];

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

    // Destination + pool output ride on the output token program.
    let destination = match (create_destination, output_token_program) {
        (true, p) if p == TOKEN_2022_PROGRAM_ID => {
            create_ata_2022(svm, &payer, &owner.pubkey(), &output_mint)
        }
        (true, _) => create_ata(svm, &payer, &owner.pubkey(), &output_mint),
        (false, p) if p == TOKEN_2022_PROGRAM_ID => derive_ata_2022(&owner.pubkey(), &output_mint),
        (false, _) => derive_ata(&owner.pubkey(), &output_mint),
    };

    let (pool_authority, _) =
        Pubkey::find_program_address(&[mock_swap::POOL_AUTHORITY_SEED], &mock_swap::ID);
    let pool_input = create_ata(svm, &payer, &pool_authority, &input_mint);
    let pool_output = if output_token_program == TOKEN_2022_PROGRAM_ID {
        let ata = create_ata_2022(svm, &payer, &pool_authority, &output_mint);
        mint_to_2022(svm, &payer, &payer, &output_mint, &ata, POOL_LIQUIDITY);
        ata
    } else {
        let ata = create_ata(svm, &payer, &pool_authority, &output_mint);
        mint_to(svm, &payer, &payer, &output_mint, &ata, POOL_LIQUIDITY);
        ata
    };

    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, FILL_ID, VAULT, ExecutionStatus::Finalized);

    Staged {
        payer,
        input_mint,
        output_mint,
        output_token_program,
        route,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        destination,
        pool_input,
        pool_output,
        pool_authority,
    }
}

/// One signed-quote VerifiedCpi action spending the whole vault, legacy output,
/// destination created.
fn staged_single(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> Staged {
    stage(
        svm,
        quoter,
        OutKind::Legacy,
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        true,
    )
}

impl Staged {
    /// The per-action remaining-accounts run for the single VerifiedCpi action:
    /// `[program, <10 template slots>]`. `destination` is overridable to model the
    /// relayer pointing the output elsewhere (redirect attack).
    fn verified_metas(&self, destination: Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // input token program (legacy)
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(self.output_mint, false),
            AccountMeta::new_readonly(self.output_token_program, false),
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(self.pool_output, false),
        ]
    }
}

// --------------------------------------------------------------------------
// Success
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_swap_succeeds() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter]).expect("verified swap succeeds");

    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn verified_cpi_sweeps_unspent_input() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    // Pull only 3/4 of the moved input; the rest must sweep back to the vault.
    let pulled = 3_000_000;
    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(pulled, out_amount), out_amount)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter]).expect("partial-pull swap succeeds");

    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        fetch_token_balance(&svm, &s.execution_vault),
        VAULT - pulled
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn verified_cpi_emits_actions_executed() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );
    let meta =
        send_instructions(&mut svm, &[exec], &s.payer, &[&quoter]).expect("verified swap succeeds");
    let event: sra_handler::events::ActionsExecuted =
        extract_anchor_event(&meta.logs).expect("ActionsExecuted emitted");
    assert_eq!(event.route, s.route);
    assert_eq!(event.fill_id, FILL_ID);
}

// --------------------------------------------------------------------------
// Outcome verification (hard revert — funds safe, receipt retryable)
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_below_min_out_reverts_funds_safe() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    // Deliver less than the signed minimum.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 2_000_000), 3_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::MinOutNotMet));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn verified_cpi_scratch_bounds_input() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    // Ask the swap to pull MORE than the engine moved into scratch — the SPL
    // transfer fails and the whole transaction reverts; the vault is untouched.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT * 2, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let res = send_instructions(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert!(res.is_err(), "pulling beyond the scratch bound must revert");
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// Account pinning (hard reject — structural)
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_destination_redirect_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    // Relayer points the Destination slot at an ATA they control.
    let attacker = Pubkey::new_unique();
    let attacker_ata = create_ata(&mut svm, &s.payer, &attacker, &s.output_mint);
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(attacker_ata),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::DestinationNotPinned));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &attacker_ata), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn verified_cpi_program_substitution_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    // Swap the pinned program account for a different (wrong) program.
    let mut metas = s.verified_metas(s.destination);
    metas[0] = AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false);
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        metas,
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::ProgramNotPinned));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// Co-signing quoter authorization (hard reject)
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_signed_quote_expired_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expired = now(&svm) - 1;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expired,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::PriceQuoteExpired));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

#[test]
fn verified_cpi_wrong_quoter_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    // A stranger co-signs instead of the route-committed quoter.
    let stranger = Keypair::new();
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(stranger.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&stranger]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

#[test]
fn verified_cpi_missing_quote_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    // A correct runtime, but no quoter account co-signs the transaction.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        None,
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_ix_expect_err(
        &mut svm,
        exec,
        &s.payer,
        &[],
        "missing co-signing quoter rejected",
    );
    assert_eq!(err, custom_error_at(0, SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

// --------------------------------------------------------------------------
// Soft fail (recoverable)
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_missing_destination_ata_soft_fails() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    // Destination ATA NOT created.
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        false,
    );
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    // Soft fail returns Ok and persists ActionsFailed; funds stay staged.
    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter])
        .expect("missing destination ATA -> ActionsFailed, not abort");
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

// --------------------------------------------------------------------------
// Composition with SplTransfer
// --------------------------------------------------------------------------

#[test]
fn spl_and_verified_cpi_compose() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let recipient = Pubkey::new_unique();
    // 30% straight transfer + 70% swapped — both off the start-of-run snapshot.
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        |input, output, q| vec![spl(recipient, 3_000), verified_cpi(input, output, 7_000, q)],
        true,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.input_mint);

    let swap_input = VAULT * 7_000 / 10_000;
    let out_amount = 1_500_000;
    let expiry = now(&svm) + 3_600;
    // remaining = [SplTransfer recipient ATA] ++ [VerifiedCpi run]
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(s.verified_metas(s.destination));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(swap_input, out_amount), out_amount)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        remaining,
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter]).expect("compose succeeds");

    assert_eq!(
        fetch_token_balance(&svm, &recipient_ata),
        VAULT * 3_000 / 10_000
    );
    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &s.scratch), 0);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

// --------------------------------------------------------------------------
// Token-2022 output handling
// --------------------------------------------------------------------------

/// A swap whose output is a (plain) Token-2022 mint lands in the owner's Token-2022
/// ATA and clears `min_out` — the output token program is read from the pinned
/// output mint, not assumed to be legacy SPL.
#[test]
fn verified_cpi_token_2022_output_succeeds() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022,
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        true,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter])
        .expect("token-2022 output swap succeeds");

    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// `min_out` is measured against the **post-fee** landed balance: a transfer-fee
/// output mint withholds its fee, and the owner's ATA receives `out_amount - fee`.
#[test]
fn verified_cpi_transfer_fee_output_uses_post_fee_balance() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022Fee {
            fee_bps: 100, // 1%
            max_fee: 1_000_000_000,
        },
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        true,
    );

    let out_amount = 1_000_000;
    let fee = 10_000; // 1% of out_amount
    let landed = out_amount - fee;
    let expiry = now(&svm) + 3_600;
    // Quote the post-fee floor; the swap nominally outputs `out_amount`.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), landed)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter])
        .expect("transfer-fee output swap succeeds at the post-fee floor");

    // The owner receives the post-fee amount, not the nominal output.
    assert_eq!(fetch_token_balance(&svm, &s.destination), landed);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// The gate compares the **post-fee** delta to `min_out`: a quote at the nominal
/// (pre-fee) output is not met once the fee is withheld → revert, funds safe.
#[test]
fn verified_cpi_transfer_fee_min_out_against_post_fee_reverts() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022Fee {
            fee_bps: 100,
            max_fee: 1_000_000_000,
        },
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        true,
    );

    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    // Quote at the nominal output; post-fee landing (990_000) falls short.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), out_amount)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::MinOutNotMet));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// A permissioned output mint (permanent delegate) is rejected unless the route
/// opted in — the owner must not silently custody a drainable RWA.
#[test]
fn verified_cpi_permissioned_output_requires_optin() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022Permissioned,
        |input, output, q| vec![verified_cpi(input, output, 10_000, q)],
        true,
    );

    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 3_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::PermissionedOutputNotAllowed));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// With `allow_permissioned_output = true`, the same permissioned mint is an
/// accepted output (the owner has explicitly taken on the issuer-custody trust).
#[test]
fn verified_cpi_permissioned_output_optin_succeeds() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022Permissioned,
        |input, output, q| {
            vec![SolanaAction::VerifiedCpi {
                program_id: mock_swap::ID,
                input_mint: input,
                input_bps: 10_000,
                account_template: mock_swap_template(),
                output_mint: output,
                quoter: q,
                allow_permissioned_output: true,
            }]
        },
        true,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter])
        .expect("opted-in permissioned output succeeds");

    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// The pinned `OutputMint` slot must hold the committed output mint — a relayer
/// cannot substitute a different mint account (which would mis-derive the output
/// token program / destination).
#[test]
fn verified_cpi_wrong_output_mint_account_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    // Replace the OutputMint slot (index 5: program, TokenProgram, SigningAuthority,
    // Source, Destination, OutputMint) with a different mint.
    let mut metas = s.verified_metas(s.destination);
    metas[5] = AccountMeta::new_readonly(s.input_mint, false);
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        metas,
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&quoter]);
    assert_eq!(err, exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// Multiple VerifiedCpi actions under one co-signing quoter
// --------------------------------------------------------------------------

/// Two 50%-input_bps VerifiedCpi actions, each swapping into its own output mint
/// through its own mock-swap pool, sharing one committed quoter.
struct TwoStaged {
    payer: Keypair,
    input_mint: Pubkey,
    output_mint_a: Pubkey,
    output_mint_b: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination_a: Pubkey,
    destination_b: Pubkey,
    pool_authority: Pubkey,
    pool_input_a: Pubkey,
    pool_output_a: Pubkey,
    pool_input_b: Pubkey,
    pool_output_b: Pubkey,
}

impl TwoStaged {
    /// `[program, <10 template slots>]` for the action swapping into `output_mint`
    /// via its own pool ATAs.
    fn verified_metas(
        &self,
        destination: Pubkey,
        output_mint: Pubkey,
        pool_input: Pubkey,
        pool_output: Pubkey,
    ) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(output_mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(pool_input, false),
            AccountMeta::new(pool_output, false),
        ]
    }
}

/// Stage a route committing two 50%-input_bps VerifiedCpi actions sharing one
/// quoter, each swapping into a distinct output mint through its own mock-swap
/// pool.
fn stage_two(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> TwoStaged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint_a = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint_b = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let action_config = vec![
        verified_cpi(input_mint, output_mint_a, 5_000, quoter.pubkey()),
        verified_cpi(input_mint, output_mint_b, 5_000, quoter.pubkey()),
    ];
    let salt = [5u8; 32];

    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: input_mint,
        action_config,
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

    let destination_a = create_ata(svm, &payer, &owner.pubkey(), &output_mint_a);
    let destination_b = create_ata(svm, &payer, &owner.pubkey(), &output_mint_b);

    let (pool_authority, pool_input_a, pool_output_a) =
        derive_mock_swap_pool(&input_mint, &output_mint_a);
    create_ata(svm, &payer, &pool_authority, &input_mint);
    create_ata(svm, &payer, &pool_authority, &output_mint_a);
    mint_to(
        svm,
        &payer,
        &payer,
        &output_mint_a,
        &pool_output_a,
        POOL_LIQUIDITY,
    );

    let (_, pool_input_b, pool_output_b) = derive_mock_swap_pool(&input_mint, &output_mint_b);
    create_ata(svm, &payer, &pool_authority, &output_mint_b);
    mint_to(
        svm,
        &payer,
        &payer,
        &output_mint_b,
        &pool_output_b,
        POOL_LIQUIDITY,
    );

    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, FILL_ID, VAULT, ExecutionStatus::Finalized);

    TwoStaged {
        payer,
        input_mint,
        output_mint_a,
        output_mint_b,
        route,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        destination_a,
        destination_b,
        pool_authority,
        pool_input_a,
        pool_output_a,
        pool_input_b,
        pool_output_b,
    }
}

/// The behavioral headline of the co-signing quoter migration: two VerifiedCpi
/// actions execute atomically in one transaction under a single quoter
/// co-signature, each landing its own swap output.
#[test]
fn two_verified_cpi_actions_one_cosigning_quoter_succeeds() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_two(&mut svm, &quoter);

    let half = VAULT / 2;
    let out_a = 1_500_000;
    let out_b = 2_500_000;
    let min_out_a = 1_400_000;
    let min_out_b = 2_400_000;
    let expiry = now(&svm) + 3_600;

    let mut remaining = s.verified_metas(
        s.destination_a,
        s.output_mint_a,
        s.pool_input_a,
        s.pool_output_a,
    );
    remaining.extend(s.verified_metas(
        s.destination_b,
        s.output_mint_b,
        s.pool_input_b,
        s.pool_output_b,
    ));

    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(half, out_a), min_out_a),
            runtime(swap_data(half, out_b), min_out_b),
        ],
        expiry,
        Some(quoter.pubkey()),
        &s.input_mint,
        remaining,
    );

    send_instructions(&mut svm, &[exec], &s.payer, &[&quoter])
        .expect("two VerifiedCpi actions under one quoter co-signature succeed");

    assert_eq!(fetch_token_balance(&svm, &s.destination_a), out_a);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), out_b);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch swept empty after both actions"
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
fn quoter_account_present_but_route_uncommitted_quoter_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    // A different signer co-signs; the route committed `quoter`, not `other`.
    let other = Keypair::new();
    let expiry = now(&svm) + 3_600;
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        expiry,
        Some(other.pubkey()),
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[exec], &s.payer, &[&other]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}
