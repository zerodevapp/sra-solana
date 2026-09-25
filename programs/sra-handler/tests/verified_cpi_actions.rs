//! Verified-CPI action execution (the shared engine + ed25519 signed-quote path +
//! Token-2022 output handling).
//!
//! Exercises the engine's security model against a generic balance-diff swap mock:
//! scratch-bounded input, pinned source/destination/authority, balance-diff
//! `≥ min_out` from an authorized signed quote, fail-closed, scratch swept empty.
//! The output may be a Token-2022 mint: the balance-diff is post-fee, and a
//! issuer-controlled or transfer-restricted Token-2022 output requires an explicit
//! opt-in. The concrete Jupiter template and the oracle `min_out` fallback
//! are follow-ups; here the swap is a mock and `min_out` is a signed quote (the
//! EVM-parity primary resolver, ed25519 on Solana).

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::TransactionError,
};
use sra_common::{
    errors::SraError,
    quote::{action_context_hash, signed_quote_message},
    types::{
        CreateRouteParams, ExecutionStatus, MinOut, SlotRole, SolanaAction, VerifiedCpiRuntime,
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
    output_token_program: Pubkey,
    input_bps: u16,
    min_out: MinOut,
) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps,
        account_template: mock_swap_template_for_output_program(output_token_program),
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
    owner: Keypair,
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
    make_actions: impl FnOnce(Pubkey, Pubkey, Pubkey, Pubkey) -> Vec<SolanaAction>,
    create_destination: bool,
) -> Staged {
    stage_with_vault_amount(
        svm,
        quoter,
        out_kind,
        VAULT,
        make_actions,
        create_destination,
    )
}

fn stage_with_vault_amount(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    out_kind: OutKind,
    vault_amount: u64,
    make_actions: impl FnOnce(Pubkey, Pubkey, Pubkey, Pubkey) -> Vec<SolanaAction>,
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
    let action_config = make_actions(
        input_mint,
        output_mint,
        output_token_program,
        quoter.pubkey(),
    );
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
    mint_to(
        svm,
        &payer,
        &payer,
        &input_mint,
        &execution_vault,
        vault_amount,
    );

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

    let fill_receipt = seed_fill_receipt_with_status(
        svm,
        &route,
        FILL_ID,
        vault_amount,
        ExecutionStatus::Finalized,
    );

    Staged {
        payer,
        owner,
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

/// One signed-quote VerifiedCpi action spending the whole fill amount, legacy output,
/// destination created.
fn staged_single(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> Staged {
    stage(
        svm,
        quoter,
        OutKind::Legacy,
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
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

const SPLIT_IN: u64 = VAULT / 2;
const SPLIT_OUT: u64 = 1_000_000;

fn with_ignored_free_slot(mut action: SolanaAction) -> SolanaAction {
    if let SolanaAction::VerifiedCpi {
        account_template, ..
    } = &mut action
    {
        account_template.slots.push(SlotRole::Free);
    }
    action
}

struct TwoSwaps {
    s: Staged,
    output_mint_b: Pubkey,
    destination_b: Pubkey,
    pool_output_b: Pubkey,
}

fn stage_two_swaps(svm: &mut litesvm::LiteSVM, quoter: &Keypair) -> TwoSwaps {
    let issuer = funded_keypair(svm, SOL);
    let output_mint_b = create_mint(svm, &issuer, &issuer.pubkey(), DECIMALS);
    let s = stage(
        svm,
        quoter,
        OutKind::Legacy,
        |input, output, program, q| {
            vec![
                with_ignored_free_slot(verified_cpi(input, output, program, 5_000, signed(q))),
                with_ignored_free_slot(verified_cpi(
                    input,
                    output_mint_b,
                    program,
                    5_000,
                    signed(q),
                )),
            ]
        },
        true,
    );
    let destination_b = create_ata(svm, &s.payer, &s.owner.pubkey(), &output_mint_b);
    let pool_output_b = create_ata(svm, &s.payer, &s.pool_authority, &output_mint_b);
    mint_to(
        svm,
        &s.payer,
        &issuer,
        &output_mint_b,
        &pool_output_b,
        POOL_LIQUIDITY,
    );
    TwoSwaps {
        s,
        output_mint_b,
        destination_b,
        pool_output_b,
    }
}

impl TwoSwaps {
    fn metas(&self, a_writes_b: bool, b_writes_a: bool) -> Vec<AccountMeta> {
        let other_mint = |pubkey, is_writable| AccountMeta {
            pubkey,
            is_writable,
            is_signer: false,
        };
        let s = &self.s;
        let mut metas = s.verified_metas(s.destination);
        metas.push(other_mint(self.output_mint_b, a_writes_b));
        metas.extend([
            AccountMeta::new_readonly(mock_swap::ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(s.scratch_authority, false),
            AccountMeta::new(s.scratch, false),
            AccountMeta::new(self.destination_b, false),
            AccountMeta::new_readonly(self.output_mint_b, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(s.input_mint, false),
            AccountMeta::new_readonly(s.pool_authority, false),
            AccountMeta::new(s.pool_input, false),
            AccountMeta::new(self.pool_output_b, false),
            other_mint(s.output_mint, b_writes_a),
        ]);
        metas
    }

    fn quotes(&self, quoter: &Keypair, expiry: i64) -> [Instruction; 2] {
        let s = &self.s;
        let message_b = signed_quote_message(
            &action_context_hash(&s.route, &FILL_ID, 1),
            &s.input_mint,
            &self.output_mint_b,
            SPLIT_IN,
            SPLIT_OUT,
            expiry,
        );
        [
            ed25519_quote_ix(quoter, &s.quote_message_at(0, SPLIT_IN, SPLIT_OUT, expiry)),
            ed25519_quote_ix(quoter, &message_b),
        ]
    }
}

fn split_runtimes(expiry: i64) -> Vec<VerifiedCpiRuntime> {
    vec![runtime(swap_data(SPLIT_IN, SPLIT_OUT), SPLIT_OUT, expiry); 2]
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
fn verified_cpi_rejects_scratch_control_mutations() {
    for mutation in [
        mock_swap::ScratchMutation::Owner,
        mock_swap::ScratchMutation::CloseAuthority,
        mock_swap::ScratchMutation::Delegate,
    ] {
        let mut svm = setup_svm();
        let quoter = Keypair::new();
        let s = staged_single(&mut svm, &quoter);
        let out_amount = 3_000_000;
        let expiry = now(&svm) + 3_600;
        let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, out_amount, expiry));
        let exec = execute_actions_ix_full(
            &s.payer.pubkey(),
            &s.route,
            FILL_ID,
            vec![runtime(
                swap_with_mutation_data(VAULT, out_amount, mutation),
                out_amount,
                expiry,
            )],
            &s.input_mint,
            s.verified_metas(s.destination),
        );

        let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
        assert_eq!(
            err,
            exec_error(SraError::ScratchControlChanged),
            "scratch mutation {mutation:?}"
        );

        let scratch = svm.get_account(&s.scratch).expect("scratch exists");
        let scratch = anchor_spl::token::spl_token::state::Account::unpack(&scratch.data)
            .expect("valid scratch token account");
        assert_eq!(scratch.owner, s.scratch_authority);
        assert_eq!(scratch.delegate, COption::None);
        assert_eq!(scratch.close_authority, COption::None);
        assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
        assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
        assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
    }
}

fn assert_zero_input_scratch_mutation_reverts(mutation: mock_swap::ScratchMutation) {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let recipient = Pubkey::new_unique();
    let s = stage_with_vault_amount(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        1,
        |input, output, program, q| {
            vec![
                spl(recipient, 9_999),
                verified_cpi(input, output, program, 1, signed(q)),
            ]
        },
        true,
    );
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient, &s.input_mint);
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message_at(1, 0, 0, expiry));
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(s.verified_metas(s.destination));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_with_mutation_data(0, 0, mutation), 0, expiry)],
        &s.input_mint,
        remaining,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ScratchControlChanged));

    let scratch = svm.get_account(&s.scratch).expect("scratch exists");
    let scratch = anchor_spl::token::spl_token::state::Account::unpack(&scratch.data)
        .expect("valid scratch token account");
    assert_eq!(scratch.owner, s.scratch_authority);
    assert_eq!(scratch.close_authority, COption::None);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 1);
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn verified_cpi_zero_input_owner_change_reverts() {
    assert_zero_input_scratch_mutation_reverts(mock_swap::ScratchMutation::Owner);
}

#[test]
fn verified_cpi_zero_input_close_authority_change_reverts() {
    assert_zero_input_scratch_mutation_reverts(mock_swap::ScratchMutation::CloseAuthority);
}

/// A callee that consumes only part of the staged input while still clearing
/// `min_out` must revert: sweeping the residue to the vault under a terminal
/// `ActionsSucceeded` receipt would leave it unrecoverable.
#[test]
fn verified_cpi_partial_input_consumption_reverts_funds_safe() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);

    // Pull only 3/4 of the moved input; the output still clears min_out.
    let pulled = 3_000_000;
    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, out_amount, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(pulled, out_amount), out_amount, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::InputNotConsumed));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &s.destination), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn verified_cpi_emits_actions_executed() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = staged_single(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
fn verified_cpi_free_slot_aliasing_output_mint_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Token2022,
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        true,
    );
    let mut metas = s.verified_metas(s.destination);
    metas[6] = AccountMeta::new(s.output_mint, false);
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        metas,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

#[test]
fn verified_cpi_writable_output_mint_of_another_action_rejected() {
    for (a_writes_b, b_writes_a) in [(true, false), (false, true)] {
        let mut svm = setup_svm();
        let quoter = Keypair::new();
        let t = stage_two_swaps(&mut svm, &quoter);
        let expiry = now(&svm) + 3_600;
        let [quote_a, quote_b] = t.quotes(&quoter, expiry);
        let exec = execute_actions_ix_full(
            &t.s.payer.pubkey(),
            &t.s.route,
            FILL_ID,
            split_runtimes(expiry),
            &t.s.input_mint,
            t.metas(a_writes_b, b_writes_a),
        );

        let err =
            send_instructions_expect_err(&mut svm, &[quote_a, quote_b, exec], &t.s.payer, &[]);
        assert_eq!(
            err,
            custom_error_at(2, SraError::ActionAccountMismatch),
            "a_writes_b {a_writes_b}"
        );
        assert_eq!(status(&svm, &t.s.fill_receipt), ExecutionStatus::Finalized);
    }
}

#[test]
fn retry_writable_output_mint_of_another_action_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let t = stage_two_swaps(&mut svm, &quoter);
    seed_fill_receipt_with_status(
        &mut svm,
        &t.s.route,
        FILL_ID,
        VAULT,
        ExecutionStatus::ActionsFailed,
    );
    let expiry = now(&svm) + 3_600;
    let [quote_a, quote_b] = t.quotes(&quoter, expiry);
    let retry = retry_actions_ix_full(
        &t.s.owner.pubkey(),
        &t.s.route,
        FILL_ID,
        split_runtimes(expiry),
        &t.s.input_mint,
        t.metas(false, true),
    );

    let err = send_instructions_expect_err(
        &mut svm,
        &[quote_a, quote_b, retry],
        &t.s.payer,
        &[&t.s.owner],
    );
    assert_eq!(err, custom_error_at(2, SraError::ActionAccountMismatch));
    assert_eq!(
        status(&svm, &t.s.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
}

#[test]
fn verified_cpi_read_only_output_mint_of_another_action_allowed() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let t = stage_two_swaps(&mut svm, &quoter);
    let expiry = now(&svm) + 3_600;
    let [quote_a, quote_b] = t.quotes(&quoter, expiry);
    let exec = execute_actions_ix_full(
        &t.s.payer.pubkey(),
        &t.s.route,
        FILL_ID,
        split_runtimes(expiry),
        &t.s.input_mint,
        t.metas(false, false),
    );

    send_instructions(&mut svm, &[quote_a, quote_b, exec], &t.s.payer, &[])
        .expect("read-only access to another action's output mint is routing");
    assert_eq!(fetch_token_balance(&svm, &t.s.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &t.s.destination), SPLIT_OUT);
    assert_eq!(fetch_token_balance(&svm, &t.destination_b), SPLIT_OUT);
    assert_eq!(
        status(&svm, &t.s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
fn verified_cpi_quote_precedes_missing_destination_soft_failure() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        false,
    );
    let expiry = now(&svm) + 3_600;
    let exec = || {
        execute_actions_ix_full(
            &s.payer.pubkey(),
            &s.route,
            FILL_ID,
            vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
            &s.input_mint,
            s.verified_metas(s.destination),
        )
    };

    let err = send_ix_expect_err(
        &mut svm,
        exec(),
        &s.payer,
        &[],
        "missing signed quote rejected before pinned-account soft failure",
    );
    assert_eq!(err, custom_error_at(0, SraError::InvalidPriceSignature));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);

    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    send_instructions(&mut svm, &[ed_ix, exec()], &s.payer, &[])
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
    // 30% straight transfer + 70% swapped from the fill amount.
    let s = stage(
        &mut svm,
        &quoter,
        OutKind::Legacy,
        |input, output, program, q| {
            vec![
                spl(recipient, 3_000),
                verified_cpi(input, output, program, 7_000, signed(q)),
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
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
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
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        true,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, min_out, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        true,
    );

    let out_amount = 1_000_000;
    let fee = 10_000; // 1% of out_amount
    let landed = out_amount - fee;
    let expiry = now(&svm) + 3_600;
    // Quote the post-fee floor; the swap nominally outputs `out_amount`.
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, landed, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), landed, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        true,
    );

    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    // Quote at the nominal output; post-fee landing (990_000) falls short.
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, out_amount, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), out_amount, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        |input, output, program, q| vec![verified_cpi(input, output, program, 10_000, signed(q))],
        true,
    );

    let expiry = now(&svm) + 3_600;
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 3_000_000), 1_000_000, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
        |input, output, program, q| {
            vec![SolanaAction::VerifiedCpi {
                program_id: mock_swap::ID,
                input_mint: input,
                input_bps: 10_000,
                account_template: mock_swap_template_for_output_program(program),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, min_out, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, out_amount), min_out, expiry)],
        &s.input_mint,
        s.verified_metas(s.destination),
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
    let ed_ix = ed25519_quote_ix(&quoter, &s.quote_message(VAULT, 1_000_000, expiry));
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        FILL_ID,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000, expiry)],
        &s.input_mint,
        metas,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}
