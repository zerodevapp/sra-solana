//! Verified-CPI action execution (the shared engine + ed25519 signed-quote path +
//! Token-2022 output handling).
//!
//! Exercises the engine's security model against a generic balance-diff swap mock:
//! scratch-bounded input, pinned source/destination/authority, balance-diff
//! `≥ min_out` from an authorized signed quote, fail-closed, scratch swept empty.
//! The output may be a Token-2022 mint: the balance-diff is post-fee, and a
//! permissioned (permanent-delegate / pausable / default-frozen) output requires an
//! explicit opt-in. The concrete Jupiter template and the oracle `min_out` fallback
//! are follow-ups; here the swap is a mock and `min_out` is a signed quote (the
//! EVM-parity primary resolver, ed25519 on Solana).

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
    quote::QuotedAction,
    types::{CreateRouteParams, ExecutionStatus, MinOut, SolanaAction},
};

const VAULT: u64 = 4_000_000;
const POOL_LIQUIDITY: u64 = 100_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_ID: [u8; 32] = [9u8; 32];
const DECIMALS: u8 = 6;

fn custom_error_at(index: u8, err: SraError) -> TransactionError {
    TransactionError::InstructionError(index, InstructionError::Custom(err.into()))
}

/// The execute_actions instruction sits at index 1 (the ed25519 quote is index 0).
fn exec_error(err: SraError) -> TransactionError {
    custom_error_at(1, err)
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
    min_out: MinOut,
) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps,
        account_template: mock_swap_template(),
        output_mint,
        min_out,
        allow_permissioned_output: false,
    }
}

fn signed(quoter: Pubkey) -> MinOut {
    MinOut::SignedQuote { quoter }
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
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

    /// The signed-quote tuple for the fill's single VerifiedCpi action (input mint,
    /// output mint, the handler-derived input amount, and the signed floor).
    fn quoted_action(&self, input_amount: u64, min_out: u64) -> QuotedAction {
        QuotedAction {
            input_mint: self.input_mint,
            output_mint: self.output_mint,
            input_amount,
            min_out,
        }
    }

    /// The aggregate signed-quote instruction covering `actions` (in action order),
    /// signed by `quoter` and bound to this route/fill/expiry.
    fn agg_quote_ix(
        &self,
        quoter: &Keypair,
        expiry: i64,
        actions: &[QuotedAction],
    ) -> solana_sdk::instruction::Instruction {
        aggregate_quote_ix(quoter, &self.route, &FILL_ID, expiry, actions)
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, min_out)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("verified swap succeeds");

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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, out_amount)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(pulled, out_amount), out_amount)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("partial-pull swap succeeds");

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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );
    let meta =
        send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("verified swap succeeds");
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 3_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 2_000_000), 3_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT * 2, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let res = send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]);
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(attacker_ata),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        metas,
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ProgramNotPinned));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// Signed-quote authorization & context binding (hard reject)
// --------------------------------------------------------------------------

#[test]
fn verified_cpi_signed_quote_expired_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expired = now(&svm) - 1;
    let ed_ix = s.agg_quote_ix(&quoter, expired, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expired,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::PriceQuoteExpired));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

#[test]
fn verified_cpi_wrong_quoter_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    // A stranger signs the (otherwise valid) quote.
    let stranger = Keypair::new();
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&stranger, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

#[test]
fn verified_cpi_min_out_tamper_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    // The quoter signs min_out = 3_000_000, but the relayer submits a runtime
    // claiming 1_000_000 (to slip a worse swap through). The reconstructed root
    // no longer matches the signature.
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 3_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

#[test]
fn verified_cpi_missing_quote_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    // A correct runtime, but NO ed25519 instruction in the transaction.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    // Only one instruction, so the error lands on index 0.
    let err = send_ix_expect_err(
        &mut svm,
        exec,
        &s.payer,
        &[],
        "missing signed quote rejected",
    );
    assert_eq!(err, custom_error_at(0, SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
}

// --------------------------------------------------------------------------
// Soft fail (recoverable) and deferred-feature gates
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        false,
    );
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    // Soft fail returns Ok and persists ActionsFailed; funds stay staged.
    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
        .expect("missing destination ATA -> ActionsFailed, not abort");
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn verified_cpi_oracle_min_out_unsupported() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    // An oracle-min_out VerifiedCpi is creatable but not yet executable.
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        |input, output, _q| {
            vec![verified_cpi(
                input,
                output,
                10_000,
                MinOut::Oracle {
                    feed: Pubkey::new_unique(),
                    max_slippage_bps: 50,
                },
            )]
        },
        true,
    );
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::UnsupportedAction));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
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
        |input, output, q| {
            vec![
                spl(recipient, 3_000),
                verified_cpi(input, output, 7_000, signed(q)),
            ]
        },
        true,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.input_mint);

    let swap_input = VAULT * 7_000 / 10_000;
    let out_amount = 1_500_000;
    let expiry = now(&svm) + 3_600;
    // The aggregate covers only the fill's VerifiedCpi action (the SplTransfer
    // contributes no quote tuple), bound to this route/fill/expiry.
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(swap_input, out_amount)]);
    // remaining = [SplTransfer recipient ATA] ++ [VerifiedCpi run]
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(s.verified_metas(s.destination));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(swap_input, out_amount), out_amount)],
        &s.input_mint,
        remaining,
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("compose succeeds");

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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        true,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, min_out)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        true,
    );

    let out_amount = 1_000_000;
    let fee = 10_000; // 1% of out_amount
    let landed = out_amount - fee;
    let expiry = now(&svm) + 3_600;
    // Quote the post-fee floor; the swap nominally outputs `out_amount`.
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, landed)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), landed)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        true,
    );

    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    // Quote at the nominal output; post-fee landing (990_000) falls short.
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, out_amount)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), out_amount)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        true,
    );

    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 3_000_000), 1_000_000)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
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
                min_out: signed(q),
                allow_permissioned_output: true,
            }]
        },
        true,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, min_out)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
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
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        metas,
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// Aggregate-quote binding across TWO VerifiedCpi actions
//
// The route commits two VerifiedCpi actions (two output mints, two pools, two
// owner destinations) sharing the single expected-mint input. One ed25519
// instruction signs the aggregate root over BOTH `QuotedAction`s, in action
// order; the handler reconstructs that same root from the committed set and its
// runtimes and verifies one signature for the whole batch.
// --------------------------------------------------------------------------

/// `floor(bps * VAULT / 10_000)` — the handler-derived input amount for a bps
/// fraction of the vault snapshot (matches the on-chain `vault_amount_for_bps`).
fn vault_amount_for_bps(bps: u16) -> u64 {
    ((VAULT as u128 * bps as u128) / 10_000u128) as u64
}

/// A route with two VerifiedCpi actions splitting the vault by `(bps_a, bps_b)`.
/// Both spend the shared legacy input; each has its own legacy output mint, its
/// own owner destination ATA, and its own pool output reserve (under the single
/// shared pool authority + input reserve). Destinations are created iff the
/// matching `create_destination_*` flag is set.
struct TwoStaged {
    payer: Keypair,
    input_mint: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    pool_authority: Pubkey,
    pool_input: Pubkey,
    input_bps_a: u16,
    input_bps_b: u16,
    output_mint_a: Pubkey,
    output_mint_b: Pubkey,
    destination_a: Pubkey,
    destination_b: Pubkey,
    pool_output_a: Pubkey,
    pool_output_b: Pubkey,
}

#[allow(clippy::too_many_arguments)]
fn stage_two(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    input_bps_a: u16,
    input_bps_b: u16,
    create_destination_a: bool,
    create_destination_b: bool,
) -> TwoStaged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint_a = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint_b = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);

    let action_config = vec![
        verified_cpi(
            input_mint,
            output_mint_a,
            input_bps_a,
            signed(quoter.pubkey()),
        ),
        verified_cpi(
            input_mint,
            output_mint_b,
            input_bps_b,
            signed(quoter.pubkey()),
        ),
    ];
    let salt = [3u8; 32];

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
    send_ix(svm, ix, &payer, &[&owner]).expect("create_route (two VerifiedCpi) should succeed");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &input_mint);
    mint_to(svm, &payer, &payer, &input_mint, &execution_vault, VAULT);

    let (scratch_authority, _) = derive_scratch_authority_pda(&route);
    let scratch = create_ata(svm, &payer, &scratch_authority, &input_mint);

    let destination_a = if create_destination_a {
        create_ata(svm, &payer, &owner.pubkey(), &output_mint_a)
    } else {
        derive_ata(&owner.pubkey(), &output_mint_a)
    };
    let destination_b = if create_destination_b {
        create_ata(svm, &payer, &owner.pubkey(), &output_mint_b)
    } else {
        derive_ata(&owner.pubkey(), &output_mint_b)
    };

    // One shared pool authority + input reserve; a distinct output reserve per mint.
    let (pool_authority, _) =
        Pubkey::find_program_address(&[mock_swap::POOL_AUTHORITY_SEED], &mock_swap::ID);
    let pool_input = create_ata(svm, &payer, &pool_authority, &input_mint);
    let pool_output_a = create_ata(svm, &payer, &pool_authority, &output_mint_a);
    mint_to(
        svm,
        &payer,
        &payer,
        &output_mint_a,
        &pool_output_a,
        POOL_LIQUIDITY,
    );
    let pool_output_b = create_ata(svm, &payer, &pool_authority, &output_mint_b);
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
        route,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        pool_authority,
        pool_input,
        input_bps_a,
        input_bps_b,
        output_mint_a,
        output_mint_b,
        destination_a,
        destination_b,
        pool_output_a,
        pool_output_b,
    }
}

impl TwoStaged {
    /// The per-action remaining-accounts run `[program, <10 template slots>]` for
    /// output `output_mint` / `destination` / `pool_output`.
    fn metas_for(
        &self,
        output_mint: Pubkey,
        destination: Pubkey,
        pool_output: Pubkey,
    ) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // input token program (legacy)
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(output_mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // output token program (legacy)
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(pool_output, false),
        ]
    }

    /// Remaining accounts for both actions, in committed action order (A then B).
    fn remaining(&self) -> Vec<AccountMeta> {
        let mut r = self.metas_for(self.output_mint_a, self.destination_a, self.pool_output_a);
        r.extend(self.metas_for(self.output_mint_b, self.destination_b, self.pool_output_b));
        r
    }

    /// The `QuotedAction` tuple for action A (input mint, output mint A, the
    /// handler-derived input amount for A's bps, and A's signed floor).
    fn quoted_a(&self, min_out: u64) -> QuotedAction {
        QuotedAction {
            input_mint: self.input_mint,
            output_mint: self.output_mint_a,
            input_amount: vault_amount_for_bps(self.input_bps_a),
            min_out,
        }
    }

    /// The `QuotedAction` tuple for action B.
    fn quoted_b(&self, min_out: u64) -> QuotedAction {
        QuotedAction {
            input_mint: self.input_mint,
            output_mint: self.output_mint_b,
            input_amount: vault_amount_for_bps(self.input_bps_b),
            min_out,
        }
    }

    /// The aggregate signed-quote instruction covering `actions` (in action order),
    /// signed by `quoter` and bound to this route/fill/expiry.
    fn agg_quote_ix(
        &self,
        quoter: &Keypair,
        expiry: i64,
        actions: &[QuotedAction],
    ) -> solana_sdk::instruction::Instruction {
        aggregate_quote_ix(quoter, &self.route, &FILL_ID, expiry, actions)
    }
}

/// Two VerifiedCpi actions, one aggregate quote over both tuples in action order:
/// both swaps land in their owner destinations, the shared scratch ends empty,
/// the whole vault is spent, and the receipt advances to ActionsSucceeded.
#[test]
fn aggregate_quote_covers_two_verified_cpi_succeeds() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_two(&mut svm, &quoter, 6_000, 4_000, true, true);

    let in_a = vault_amount_for_bps(6_000);
    let in_b = vault_amount_for_bps(4_000);
    let out_a = 2_000_000;
    let out_b = 1_200_000;
    let min_a = 1_900_000;
    let min_b = 1_100_000;
    let expiry = now(&svm) + 3_600;

    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_a(min_a), s.quoted_b(min_b)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(in_a, out_a), min_a),
            runtime(swap_data(in_b, out_b), min_b),
        ],
        &s.input_mint,
        s.remaining(),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
        .expect("two-action aggregate-quoted batch succeeds");

    assert_eq!(fetch_token_balance(&svm, &s.destination_a), out_a);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), out_b);
    // 6000 + 4000 = 100% of the snapshot; the whole vault is spent.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "shared scratch swept empty"
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// Each action's `min_out` is bound individually: the quoter signs both floors,
/// but action A's swap delivers below A's floor. The tx reverts at the execute ix
/// with MinOutNotMet, no destination receives anything, the vault is untouched,
/// and the receipt stays Finalized (retryable).
#[test]
fn aggregate_quote_enforces_per_action_min_out() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_two(&mut svm, &quoter, 5_000, 5_000, true, true);

    let in_a = vault_amount_for_bps(5_000);
    let in_b = vault_amount_for_bps(5_000);
    // A is signed for a 2_000_000 floor but delivers only 1_000_000; B is fine.
    let min_a = 2_000_000;
    let min_b = 1_000_000;
    let out_a = 1_000_000; // below A's signed floor
    let out_b = 1_500_000;
    let expiry = now(&svm) + 3_600;

    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_a(min_a), s.quoted_b(min_b)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(in_a, out_a), min_a),
            runtime(swap_data(in_b, out_b), min_b),
        ],
        &s.input_mint,
        s.remaining(),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::MinOutNotMet));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination_a), 0);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// A quote signed over set A's tuples cannot be replayed against a route committed
/// to set B: the handler reconstructs the root from B's committed output mints, so
/// the A-root signature no longer matches -> InvalidPriceSignature, funds safe.
#[test]
#[allow(non_snake_case)]
fn aggregate_quote_for_set_A_rejected_for_set_B() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    // The live route (set B).
    let s = stage_two(&mut svm, &quoter, 6_000, 4_000, true, true);

    let in_a = vault_amount_for_bps(6_000);
    let in_b = vault_amount_for_bps(4_000);
    let min_a = 1_000_000;
    let min_b = 1_000_000;
    let expiry = now(&svm) + 3_600;

    // Set A: identical economics, but different output mints. The quoter signs A's
    // root; the handler will reconstruct B's (the committed output mints differ).
    let foreign_out_a = Pubkey::new_unique();
    let foreign_out_b = Pubkey::new_unique();
    let set_a = [
        QuotedAction {
            input_mint: s.input_mint,
            output_mint: foreign_out_a,
            input_amount: in_a,
            min_out: min_a,
        },
        QuotedAction {
            input_mint: s.input_mint,
            output_mint: foreign_out_b,
            input_amount: in_b,
            min_out: min_b,
        },
    ];
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &set_a);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(in_a, 1_500_000), min_a),
            runtime(swap_data(in_b, 1_500_000), min_b),
        ],
        &s.input_mint,
        s.remaining(),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination_a), 0);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// Order is bound: a quote signed with the two tuples reversed vs the committed
/// action order does not match the handler's in-order reconstruction ->
/// InvalidPriceSignature, funds safe.
#[test]
fn aggregate_quote_wrong_action_order_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_two(&mut svm, &quoter, 6_000, 4_000, true, true);

    let in_a = vault_amount_for_bps(6_000);
    let in_b = vault_amount_for_bps(4_000);
    let min_a = 1_000_000;
    let min_b = 1_000_000;
    let expiry = now(&svm) + 3_600;

    // Sign B-then-A; the handler reconstructs A-then-B (committed order).
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_b(min_b), s.quoted_a(min_a)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(in_a, 1_500_000), min_a),
            runtime(swap_data(in_b, 1_500_000), min_b),
        ],
        &s.input_mint,
        s.remaining(),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination_a), 0);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// The aggregate quote carries a batch-wide expiry: an already-elapsed expiry is
/// rejected up front with PriceQuoteExpired, before any transfer.
#[test]
fn aggregate_quote_expired_batch_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_two(&mut svm, &quoter, 6_000, 4_000, true, true);

    let in_a = vault_amount_for_bps(6_000);
    let in_b = vault_amount_for_bps(4_000);
    let min_a = 1_000_000;
    let min_b = 1_000_000;
    let expired = now(&svm) - 1;

    let ed_ix = s.agg_quote_ix(&quoter, expired, &[s.quoted_a(min_a), s.quoted_b(min_b)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![
            runtime(swap_data(in_a, 1_500_000), min_a),
            runtime(swap_data(in_b, 1_500_000), min_b),
        ],
        &s.input_mint,
        s.remaining(),
        expired,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::PriceQuoteExpired));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination_a), 0);
    assert_eq!(fetch_token_balance(&svm, &s.destination_b), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// On-chain config read (no action_config argument)
// --------------------------------------------------------------------------

/// execute_actions takes no `action_config` argument — the committed config is read
/// from the route account. A happy single-VerifiedCpi run (built purely from the
/// route-derived accounts, with no config passed in the instruction data) succeeds:
/// the positive baseline for the config-read mutation proof.
#[test]
fn execute_reads_route_action_config() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, min_out)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out)],
        &s.input_mint,
        s.verified_metas(s.destination),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[])
        .expect("config read from the route drives execution");

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

/// The relayer-supplied remaining_accounts must match the route's committed action
/// shape. Here the route commits ONE VerifiedCpi (11 slots: program + 10 template),
/// but the run supplies only the SplTransfer-shaped single account — a different
/// action shape — so validation against the committed template rejects it with
/// ActionAccountMismatch, funds safe. (Replaces the old config-hash-mismatch test:
/// the config is no longer hashed in, it is read from the route.)
#[test]
fn execute_rejects_mismatched_remaining_accounts() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    // A single account where the committed VerifiedCpi expects [program, 10 slots]:
    // the account count for that action shape can never be satisfied.
    let wrong_remaining = vec![AccountMeta::new(s.destination, false)];
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        wrong_remaining,
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}
