//! Verified-CPI action execution (the shared engine + ed25519 signed-quote path).
//!
//! Exercises the engine's security model against a generic balance-diff swap mock:
//! scratch-bounded input, pinned source/destination/authority, balance-diff
//! `≥ min_out` from an authorized signed quote, fail-closed, scratch swept empty.
//! The concrete Jupiter template, the oracle `min_out` fallback, and Token-2022
//! output handling are follow-ups; here the swap is a mock and `min_out` is a
//! signed quote (the EVM-parity primary resolver, ed25519 on Solana).

mod common;

use anchor_lang::InstructionData;
use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_sdk::{
    clock::Clock,
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::TransactionError,
};
use sra_common::{
    errors::SraError,
    quote::{action_context_hash, signed_quote_message},
    types::{
        AccountTemplate, CreateRouteParams, ExecutionStatus, FixedRole, MinOut, SlotRole,
        SolanaAction, VerifiedCpiRuntime,
    },
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

/// The mock-swap account template, in the mock's `Swap` account order: four pinned
/// Fixed slots then the relayer-supplied routing (mints + pool accounts).
fn mock_swap_template() -> AccountTemplate {
    AccountTemplate {
        slots: vec![
            SlotRole::Fixed(FixedRole::TokenProgram),
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Free, // input_mint
            SlotRole::Free, // output_mint
            SlotRole::Free, // pool_authority
            SlotRole::Free, // pool_input
            SlotRole::Free, // pool_output
        ],
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

fn spl(recipient: Pubkey, amount_bps: u16) -> SolanaAction {
    SolanaAction::SplTransfer {
        recipient,
        amount_bps,
    }
}

struct Staged {
    payer: Keypair,
    input_mint: Pubkey,
    output_mint: Pubkey,
    route: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination: Pubkey,
    pool_input: Pubkey,
    pool_output: Pubkey,
    pool_authority: Pubkey,
    action_config: Vec<SolanaAction>,
}

/// Create the input/output mints, build `action_config` from them (and `quoter`),
/// create the route, fund the execution vault, create + fund the mock pool, create
/// the scratch ATA, and seed a `Finalized` receipt. The destination ATA is created
/// iff `create_destination`.
fn stage(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    make_actions: impl FnOnce(Pubkey, Pubkey, Pubkey) -> Vec<SolanaAction>,
    create_destination: bool,
) -> Staged {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let action_config = make_actions(input_mint, output_mint, quoter.pubkey());
    let salt = [2u8; 32];

    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: input_mint,
        app_id: [0x55u8; 32],
        action_config: action_config.clone(),
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
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

    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, FILL_ID, VAULT, ExecutionStatus::Finalized);

    Staged {
        payer,
        input_mint,
        output_mint,
        route,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        destination,
        pool_input,
        pool_output,
        pool_authority,
        action_config,
    }
}

/// One signed-quote VerifiedCpi action spending the whole vault, destination created.
fn staged_single(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> Staged {
    stage(
        svm,
        quoter,
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        true,
    )
}

impl Staged {
    /// The per-action remaining-accounts run for the single VerifiedCpi action:
    /// `[program, <9 template slots>]`. `destination` is overridable to model the
    /// relayer pointing the output elsewhere (redirect attack).
    fn verified_metas(&self, destination: Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.scratch_authority, false),
            AccountMeta::new(self.scratch, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(self.input_mint, false),
            AccountMeta::new_readonly(self.output_mint, false),
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(self.pool_output, false),
        ]
    }

    /// Reconstruct the signed-quote message for the single VerifiedCpi at action 0.
    fn quote_message(&self, input_amount: u64, min_amount_out: u64, expiry: i64) -> Vec<u8> {
        self.quote_message_at(0, input_amount, min_amount_out, expiry)
    }

    /// Reconstruct the signed-quote message for the VerifiedCpi at `action_index`.
    fn quote_message_at(
        &self,
        action_index: u8,
        input_amount: u64,
        min_amount_out: u64,
        expiry: i64,
    ) -> Vec<u8> {
        let context_hash = action_context_hash(&self.route, &FILL_ID, action_index);
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

fn now(svm: &litesvm::LiteSVM) -> i64 {
    svm.get_sysvar::<Clock>().unix_timestamp
}

/// `inner_data` for the mock swap: pull `in_amount`, deliver `out_amount`.
fn swap_data(in_amount: u64, out_amount: u64) -> Vec<u8> {
    mock_swap::instruction::Swap {
        in_amount,
        out_amount,
    }
    .data()
}

fn runtime(inner_data: Vec<u8>, min_amount_out: u64, expiry: i64) -> VerifiedCpiRuntime {
    VerifiedCpiRuntime {
        inner_data,
        min_amount_out,
        expiry,
    }
}

fn status(svm: &litesvm::LiteSVM, fill_receipt: &Pubkey) -> ExecutionStatus {
    fetch_fill_receipt(svm, fill_receipt).execution_status
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, min_out, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, out_amount), min_out, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, out_amount, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(pulled, out_amount), out_amount, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 3_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 2_000_000), 3_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT * 2, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(attacker_ata),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        metas,
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expired));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expired)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&stranger, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    // claiming 1_000_000 (to slip a worse swap through). The reconstructed message
    // no longer matches the signature.
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 3_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        |input, output, q| vec![verified_cpi(input, output, 10_000, signed(q))],
        false,
    );
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    // The VerifiedCpi is the second action (index 1) — the quote binds to that index.
    let ed_ix = ed25519_quote_ix(
        &quoter,
        &s.quote_message_at(1, swap_input, out_amount, expiry),
    );
    // remaining = [SplTransfer recipient ATA] ++ [VerifiedCpi run]
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(s.verified_metas(s.destination));
    let exec = execute_actions_ix_full(
        &s.route,
        FILL_ID,
        s.action_config.clone(),
        vec![runtime(
            swap_data(swap_input, out_amount),
            out_amount,
            expiry,
        )],
        &s.input_mint,
        remaining,
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
