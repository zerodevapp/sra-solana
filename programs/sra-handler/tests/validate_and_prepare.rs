mod common;

use common::*;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};
use sra_cctp_adapter::CctpAdapterError;
use sra_common::{
    abi::u256_from_u64,
    constants::{FINALITY_THRESHOLD_FAST, FINALITY_THRESHOLD_FINALIZED},
    errors::SraError,
    source_deposit::{CctpDepositParams, PrepareParams},
};

const INPUT: u64 = 1_000_000;
// 50 bps slippage tolerance -> min acceptable = 995_000 for INPUT = 1_000_000.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;

fn default_prepare_params(fx: &ValidatePrepareFixture) -> PrepareParams {
    sample_prepare_params(fx.target_token)
}

/// The paired 2-IX flow with caller-chosen CCTP execution params (the fixture
/// default is a no-fee standard transfer); rejections land at IX 1 with the
/// adapter's own error.
fn invoke_with_cctp_params(
    svm: &mut litesvm::LiteSVM,
    fx: &ValidatePrepareFixture,
    prepare: PrepareParams,
    cctp: CctpDepositParams,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    let (execute_ix, event_data) = execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp);
    let address_book_entry = fx.address_book_entry(prepare.target_chain_id, &prepare.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare,
        &execute_ix,
    );
    send_instructions(
        svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
}

fn adapter_error_at_1(err: TransactionError, expected: CctpAdapterError, context: &str) {
    assert_eq!(
        err,
        TransactionError::InstructionError(1, InstructionError::Custom(expected.into())),
        "{context}",
    );
}

#[test]
fn validate_and_prepare_happy_path() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let supply_before = fetch_mint_supply(&svm, &fx.source_mint);
    let params = default_prepare_params(&fx);
    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("validate_and_prepare should succeed");

    // Tokens flowed owner -> SRA vault -> adapter custody -> CCTP burn in a single tx.
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - INPUT,
        "owner ATA debited",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.sra_vault),
        0,
        "SRA vault is transient — tokens pass through in the same tx",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        0,
        "adapter custody is drained by IX 2 in the same tx",
    );
    assert_eq!(
        fetch_mint_supply(&svm, &fx.source_mint),
        supply_before - INPUT,
        "the bridged amount is burned, not vaulted",
    );

    // Nonce bumped 0 -> 1.
    let nonce_state = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
    assert_eq!(nonce_state.nonce, 1);

    // DepositPrepared event emitted with raw bytes32 routing inputs.
    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event must be emitted");
    assert_eq!(ev.sra_config, fx.sra_config);
    assert_eq!(ev.source_mint, fx.source_mint);
    assert_eq!(ev.adapter_program, fx.adapter_program);
    assert_eq!(ev.nonce, 0, "event carries the pre-increment nonce");
    assert_eq!(ev.input_amount, INPUT);
    assert_eq!(ev.output_amount, u256_from_u64(OUTPUT_WITHIN_SLIPPAGE));
    assert_eq!(ev.target_chain_id, params.target_chain_id);
    assert_eq!(ev.recipient, params.recipient);
    assert_eq!(ev.target_token, params.target_token);
}

#[test]
fn validate_and_prepare_succeeds_with_decimal_only_surface() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let supply_before = fetch_mint_supply(&svm, &fx.source_mint);
    fx.invoke(&mut svm, default_prepare_params(&fx))
        .expect("deposits should still succeed with the decimal-only surface");

    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - INPUT,
        "owner ATA debited",
    );
    assert_eq!(
        fetch_mint_supply(&svm, &fx.source_mint),
        supply_before - INPUT,
        "bridged funds are burned",
    );
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 1);
}

#[test]
fn execute_deposit_emits_cctp_deposit_executed() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("paired source flow should succeed");

    let ev = extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&meta.logs)
        .expect("CctpDepositExecuted event must be emitted");
    assert_eq!(ev.sra_config, fx.sra_config);
    assert_eq!(ev.input_mint, fx.source_mint);
    assert_eq!(ev.nonce, 0);
    assert_eq!(ev.destination_domain, 3, "42161 maps to CCTP domain 3");
    assert_eq!(ev.input_amount, INPUT);
    assert_eq!(ev.output_amount, params.output_amount);
    assert_eq!(ev.target_token, params.target_token);
    assert_eq!(ev.max_fee, 0);
    assert_eq!(ev.min_finality_threshold, FINALITY_THRESHOLD_FINALIZED);

    // The mock recorded exactly what the adapter CPI'd: the burn of the net
    // toward the attested recipient, permissionless receive.
    let sent = fetch_sent_deposit_for_burn(&svm, &ev.message_sent_event_data);
    assert_eq!(sent.amount, INPUT);
    assert_eq!(sent.destination_domain, 3);
    assert_eq!(
        sent.mint_recipient,
        Pubkey::new_from_array(params.recipient)
    );
    assert_eq!(sent.destination_caller, Pubkey::default());
    assert_eq!(sent.max_fee, 0);
    assert_eq!(sent.min_finality_threshold, FINALITY_THRESHOLD_FINALIZED);
}

#[test]
fn validate_and_prepare_standalone_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let prepare = default_prepare_params(&fx);
    let (execute_ix, _event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare,
        &execute_ix,
    );
    let err = send_ix(&mut svm, ix, &fx.payer, &[&fx.owner])
        .expect_err("standalone ix1 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::MissingPairedInstruction.into()),
        )
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_program_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let (mut bad_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&params));
    bad_ix.program_id = PROGRAM_ID;

    let address_book_entry = fx.address_book_entry(params.target_chain_id, &params.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        params,
        &bad_ix,
    );
    let err = send_instructions(
        &mut svm,
        &[validate_ix, bad_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("wrong next program must reject")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidPairedInstruction.into()),
        )
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_routing_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    // Diverge a routing field the adapter re-derives from the attested prepare,
    // so the adapter's `expected_deposit == *deposit` check rejects it.
    let mut cctp = cctp_params_for(&prepare);
    cctp.recipient = evm_bytes32(0x99);

    let err = invoke_with_cctp_params(&mut svm, &fx, prepare, cctp)
        .expect_err("routing mismatch must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(SraError::PairedInstructionMismatch.into()),
        )
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_sra_config_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);

    let address_book_entry = fx.address_book_entry(prepare.target_chain_id, &fx.target_token);
    let (mut execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));
    // ADAPTER_ACCT_SRA_CONFIG = 2 in the adapter ix layout.
    execute_ix.accounts[2].pubkey = solana_sdk::pubkey::Pubkey::new_unique();
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare.clone(),
        &execute_ix,
    );

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("wrong sra_config slot in IX 2 must reject")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::PairedInstructionMismatch.into()),
        )
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_source_mint_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);

    let address_book_entry = fx.address_book_entry(prepare.target_chain_id, &fx.target_token);
    let (mut execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));
    // ADAPTER_ACCT_INPUT_MINT = 6 in the adapter ix layout.
    execute_ix.accounts[6].pubkey = solana_sdk::pubkey::Pubkey::new_unique();
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare.clone(),
        &execute_ix,
    );

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("wrong input_mint slot in IX 2 must reject")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::PairedInstructionMismatch.into()),
        )
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_max_fee_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    let base_cctp = cctp_params_for(&prepare);
    let (paired_execute_ix, _paired_event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &base_cctp);

    let address_book_entry = fx.address_book_entry(prepare.target_chain_id, &fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare.clone(),
        &paired_execute_ix,
    );

    // The executed adapter leg claims a different max_fee than the attested one.
    let mut mutated_cctp = base_cctp;
    mutated_cctp.max_fee = 500;
    let (execute_ix, event_data) = execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &mutated_cctp);

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("max_fee mismatch must reject")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::PairedInstructionMismatch.into()),
        )
    );
}

#[test]
fn validate_and_prepare_malformed_next_ix_data_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);

    let address_book_entry = fx.address_book_entry(prepare.target_chain_id, &fx.target_token);
    let (mut execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));
    // Wrong discriminator: zero out the leading 8 bytes so decode rejects
    // before any field comparison.
    for byte in execute_ix.data.iter_mut().take(8) {
        *byte = 0;
    }
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        None,
        fx.handler_data.clone(),
        prepare.clone(),
        &execute_ix,
    );

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("malformed IX 2 data must reject")
    .err
    .clone();

    assert!(matches!(err, TransactionError::InstructionError(1, _)));
}

#[test]
fn validate_and_prepare_non_adjacent_next_ix_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke_with(
            &mut svm,
            default_prepare_params(&fx),
            vec![compute_budget_ix()],
        )
        .expect_err("non-adjacent ix2 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidPairedInstruction.into()),
        )
    );
}

#[test]
fn execute_deposit_standalone_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    seed_mint_nonce_account(
        &mut svm,
        &fx.accounts.mint_nonce,
        1,
        derive_mint_nonce_pda(&fx.sra_config, &fx.source_mint).1,
    );
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );

    let prepare = default_prepare_params(&fx);
    let err = invoke_execute_only(&fx, &mut svm, &cctp_params_for(&prepare))
        .expect_err("standalone ix2 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::MissingPairedInstruction.into()),
        )
    );
}

#[test]
fn execute_deposit_wrong_prev_ix_program_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    seed_mint_nonce_account(
        &mut svm,
        &fx.accounts.mint_nonce,
        1,
        derive_mint_nonce_pda(&fx.sra_config, &fx.source_mint).1,
    );
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );

    let prepare = default_prepare_params(&fx);
    let (execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));

    let err = send_instructions(
        &mut svm,
        &[compute_budget_ix(), execute_ix],
        &fx.payer,
        &[&event_data],
    )
    .expect_err("non-handler prev ix must reject in adapter")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(SraError::InvalidPairedInstruction.into()),
        )
    );
}

#[test]
fn execute_deposit_wrong_prev_handler_call_rejected() {
    // Use a real but different sra_handler instruction (`set_address_book_entry`) as the
    // preceding ix. The adapter verifier passes the program_id check but rejects
    // on the account-count guard since set_address_book_entry has fewer accounts than
    // validate_and_prepare's adapter_program slot index.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    seed_mint_nonce_account(
        &mut svm,
        &fx.accounts.mint_nonce,
        1,
        derive_mint_nonce_pda(&fx.sra_config, &fx.source_mint).1,
    );
    create_ata(
        &mut svm,
        &fx.payer,
        &fx.accounts.adapter_authority,
        &fx.source_mint,
    );

    let prepare = default_prepare_params(&fx);
    let other_handler_ix = set_address_book_entry_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.address_book_root,
        &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
        fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        6,
    );
    let (execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), &fx, &cctp_params_for(&prepare));

    let err = send_instructions(
        &mut svm,
        &[other_handler_ix, execute_ix],
        &fx.payer,
        &[&fx.owner, &event_data],
    )
    .expect_err("non-validate_and_prepare handler ix must reject in adapter")
    .err
    .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(SraError::InvalidPairedInstruction.into()),
        )
    );
}

#[test]
fn execute_deposit_cctp_quote_boundary() {
    // The committed quote must survive the worst-case CCTP fee:
    // net − max_fee ≥ output. INPUT = 1_000_000 (no protocol fee) with
    // OUTPUT_WITHIN_SLIPPAGE = 999_500 leaves exactly 500 of fee headroom.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);

    let mut cctp = cctp_params_for(&prepare);
    cctp.max_fee = 500;
    invoke_with_cctp_params(&mut svm, &fx, prepare.clone(), cctp)
        .expect("net − max_fee == output must pass");

    let mut cctp = cctp_params_for(&prepare);
    cctp.max_fee = 501;
    let err = invoke_with_cctp_params(&mut svm, &fx, prepare.clone(), cctp)
        .expect_err("one above the fee headroom must reject")
        .err
        .clone();
    adapter_error_at_1(
        err,
        CctpAdapterError::QuoteExceedsNetAfterFee,
        "output above net − max_fee must reject",
    );

    let mut cctp = cctp_params_for(&prepare);
    cctp.max_fee = INPUT + 1;
    let err = invoke_with_cctp_params(&mut svm, &fx, prepare, cctp)
        .expect_err("max_fee above the net must fail closed")
        .err
        .clone();
    adapter_error_at_1(
        err,
        CctpAdapterError::QuoteExceedsNetAfterFee,
        "max_fee above the net must fail closed",
    );
}

#[test]
fn execute_deposit_cctp_finality_threshold_bounds() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);

    for rejected in [0u32, 1, 999, 1001, 1999, 2001] {
        let mut cctp = cctp_params_for(&prepare);
        cctp.min_finality_threshold = rejected;
        let err = invoke_with_cctp_params(&mut svm, &fx, prepare.clone(), cctp)
            .expect_err("out-of-bounds threshold must reject")
            .err
            .clone();
        adapter_error_at_1(
            err,
            CctpAdapterError::InvalidFinalityThreshold,
            "noncanonical threshold must reject",
        );
    }

    for accepted in [FINALITY_THRESHOLD_FAST, FINALITY_THRESHOLD_FINALIZED] {
        let mut cctp = cctp_params_for(&prepare);
        cctp.min_finality_threshold = accepted;
        invoke_with_cctp_params(&mut svm, &fx, prepare.clone(), cctp)
            .expect("Fast and Standard must be accepted");
    }
}

#[test]
fn execute_deposit_cctp_fast_transfer_happy_path() {
    // Fast transfer: soft finality plus a positive fee cap, quoted inside the
    // fee headroom. The mock records both execution fields verbatim.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = PrepareParams {
        output_amount: u256_from_u64(999_000),
        ..default_prepare_params(&fx)
    };
    let mut cctp = cctp_params_for(&prepare);
    cctp.max_fee = 1_000;
    cctp.min_finality_threshold = FINALITY_THRESHOLD_FAST;

    let meta = invoke_with_cctp_params(&mut svm, &fx, prepare, cctp)
        .expect("fast transfer within the fee headroom must pass");

    let ev = extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&meta.logs)
        .expect("CctpDepositExecuted event must be emitted");
    assert_eq!(ev.max_fee, 1_000);
    assert_eq!(ev.min_finality_threshold, FINALITY_THRESHOLD_FAST);
    let sent = fetch_sent_deposit_for_burn(&svm, &ev.message_sent_event_data);
    assert_eq!(sent.max_fee, 1_000);
    assert_eq!(sent.min_finality_threshold, FINALITY_THRESHOLD_FAST);
}

#[test]
fn execute_deposit_cctp_unsupported_chain_rejected() {
    // BNB (56) is the deliberate omission from the CCTP domain table. The
    // handler is bridge-agnostic and passes it through; the adapter rejects.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new_with_target_chain(&mut svm, 56);
    let prepare = PrepareParams {
        target_chain_id: 56,
        ..sample_prepare_params(fx.target_token)
    };

    let err = invoke_with_cctp_params(&mut svm, &fx, prepare.clone(), cctp_params_for(&prepare))
        .expect_err("unmapped chain must reject")
        .err
        .clone();

    adapter_error_at_1(
        err,
        CctpAdapterError::UnsupportedChain,
        "unmapped target chain must reject in the adapter",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "rejection reverts the whole paired tx",
    );
}

#[test]
fn validate_and_prepare_disabled_address_book_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        0,
    );

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("disabled address book must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAddressBook.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_unregistered_adapter_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new_with_adapter_policy(
        &mut svm,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![],
    );

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("unregistered adapter must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::AdapterNotAllowed.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_non_executable_adapter_rejected() {
    let mut svm = setup_svm();
    let non_executable_adapter = funded_keypair(&mut svm, SOL);
    let fx = ValidatePrepareFixture::new_with_adapter_policy(
        &mut svm,
        non_executable_adapter.pubkey(),
        50,
        vec![non_executable_adapter.pubkey()],
    );

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("non-executable adapter must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAdapterProgram.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_wrong_handler_data_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let mut wrong_handler_data = fx.handler_data.clone();
    wrong_handler_data.allowed_adapters.clear();

    let err = fx
        .invoke_with_handler_data(
            &mut svm,
            wrong_handler_data,
            default_prepare_params(&fx),
            vec![],
        )
        .expect_err("altered handler_data must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidHandlerData.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_wrong_address_book_root_in_handler_data_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let mut wrong_handler_data = fx.handler_data.clone();
    wrong_handler_data.address_book_root = Pubkey::new_unique();

    let err = fx
        .invoke_with_handler_data(
            &mut svm,
            wrong_handler_data,
            default_prepare_params(&fx),
            vec![],
        )
        .expect_err("mismatched address_book_root must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidHandlerData.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_slippage_exceeded_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                output_amount: u256_from_u64(900_000),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("slippage must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::SlippageExceeded.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn decimal_normalized_output_passes_handler_but_cctp_rejects_quote() {
    // An 18-decimal target output within slippage clears the handler's
    // decimal-normalized floor (IX 0 succeeds — the error index proves it),
    // but the CCTP adapter's raw-unit quote invariant rejects it: CCTP is
    // USDC-only and 1:1, so a corridor whose target token out-scales the net
    // input cannot be quoted through this adapter.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        18,
    );

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: 1_000_000,
                output_amount: u256_from_u64(995_000_000_000_000_000),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("out-scaled output must reject in the adapter")
        .err
        .clone();

    adapter_error_at_1(
        err,
        CctpAdapterError::QuoteExceedsNetAfterFee,
        "handler accepts the normalized output; the CCTP quote check rejects it",
    );
}

#[test]
fn validate_and_prepare_rejects_decimal_normalized_slippage_below_boundary() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        18,
    );

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: 1_000_000,
                output_amount: u256_from_u64(994_999_999_999_999_999),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("decimal-normalized slippage below boundary must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::SlippageExceeded.into()),
        )
    );
}

#[test]
fn validate_and_prepare_handles_large_source_decimal_gap_without_overflow() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new_with_decimals_and_adapter_policy(
        &mut svm,
        45,
        6,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![CCTP_ADAPTER_PROGRAM_ID],
    );

    fx.invoke(
        &mut svm,
        PrepareParams {
            input_amount: 1_000_000,
            output_amount: u256_from_u64(1),
            ..default_prepare_params(&fx)
        },
    )
    .expect("large source decimal gaps should not overflow the slippage check");
}

#[test]
fn validate_and_prepare_large_target_decimal_gap_rejects_as_slippage() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new_with_decimals_and_adapter_policy(
        &mut svm,
        6,
        45,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![CCTP_ADAPTER_PROGRAM_ID],
    );

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: 1,
                output_amount: u256_from_u64(u64::MAX),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("unrepresentable target decimal gaps must reject as slippage")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::SlippageExceeded.into()),
        )
    );
}

#[test]
fn validate_and_prepare_insufficient_balance_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let over_balance = OWNER_STARTING_BALANCE + 1;
    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: over_balance,
                output_amount: u256_from_u64(over_balance),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("over-balance transfer must fail")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::Custom(1),),
        "SPL Token's TokenError::InsufficientFunds (= Custom(1))",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_sequential_nonce_increments() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let supply_before = fetch_mint_supply(&svm, &fx.source_mint);
    let mut params = default_prepare_params(&fx);
    params.input_amount = 100_000;
    params.output_amount = u256_from_u64(99_500);

    for expected_pre_nonce in 0u64..3 {
        let meta = fx
            .invoke(&mut svm, params.clone())
            .unwrap_or_else(|_| panic!("deposit {expected_pre_nonce} should succeed"));

        let ev = extract_deposit_prepared(&meta.logs).expect("event");
        assert_eq!(
            ev.nonce, expected_pre_nonce,
            "deposit #{expected_pre_nonce} carries pre-increment nonce",
        );
        assert_eq!(ev.target_token, params.target_token);

        let n = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
        assert_eq!(
            n.nonce,
            expected_pre_nonce + 1,
            "nonce advances after each deposit",
        );
    }

    // After 3 successful deposits: 3x 100_000 burned, owner is short the same.
    assert_eq!(
        fetch_mint_supply(&svm, &fx.source_mint),
        supply_before - 300_000
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - 300_000
    );
}

#[test]
fn validate_and_prepare_slippage_boundary_succeeds() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let min_acceptable = 995_000;
    let params = PrepareParams {
        output_amount: u256_from_u64(min_acceptable),
        ..default_prepare_params(&fx)
    };

    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("boundary slippage output must succeed");

    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event");
    assert_eq!(ev.output_amount, u256_from_u64(min_acceptable));
    assert_eq!(ev.nonce, 0);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 1);
}

#[test]
fn validate_and_prepare_concurrent_sras_same_adapter_independent() {
    let mut svm = setup_svm();
    let fx_a = ValidatePrepareFixture::new(&mut svm);
    let fx_b = ValidatePrepareFixture::new(&mut svm);

    assert_ne!(fx_a.owner.pubkey(), fx_b.owner.pubkey());
    assert_eq!(fx_a.adapter_program, fx_b.adapter_program);

    let meta_a = fx_a
        .invoke(&mut svm, default_prepare_params(&fx_a))
        .expect("first owner deposit should succeed");
    let meta_b = fx_b
        .invoke(&mut svm, default_prepare_params(&fx_b))
        .expect("second owner deposit should succeed");

    let ev_a = extract_deposit_prepared(&meta_a.logs).expect("first DepositPrepared event");
    let ev_b = extract_deposit_prepared(&meta_b.logs).expect("second DepositPrepared event");
    assert_eq!(ev_a.adapter_program, fx_a.adapter_program);
    assert_eq!(ev_b.adapter_program, fx_b.adapter_program);
    assert_eq!(ev_a.nonce, 0);
    assert_eq!(ev_b.nonce, 0);
    assert_eq!(fetch_mint_nonce(&svm, &fx_a.accounts.mint_nonce).nonce, 1);
    assert_eq!(fetch_mint_nonce(&svm, &fx_b.accounts.mint_nonce).nonce, 1);
}

#[test]
fn validate_and_prepare_multi_mint_single_sra_independent_nonces() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let meta_a0 = fx
        .invoke(&mut svm, params.clone())
        .expect("first mint deposit #0 should succeed");
    let ev_a0 = extract_deposit_prepared(&meta_a0.logs).expect("first mint event #0");

    let second_mint = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 6);
    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &second_mint,
        fx.target_chain_id,
        fx.target_token,
        6,
    );
    let second_accounts =
        ValidatePrepareAccounts::derive(&fx.sra_config, &second_mint, &fx.adapter_program);
    let second_owner_ata = create_ata(&mut svm, &fx.payer, &fx.owner.pubkey(), &second_mint);
    let second_across_state = funded_keypair(&mut svm, SOL).pubkey();
    let second_across_vault = create_ata(&mut svm, &fx.payer, &second_across_state, &second_mint);
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &second_mint,
        &second_owner_ata,
        OWNER_STARTING_BALANCE,
    );
    let second_fx = ValidatePrepareFixture {
        payer: fx.payer.insecure_clone(),
        owner: fx.owner.insecure_clone(),
        fee_admin: fx.fee_admin.insecure_clone(),
        address_book_root: fx.address_book_root,
        handler_data: fx.handler_data.clone(),
        sra_config: fx.sra_config,
        source_mint: second_mint,
        target_chain_id: fx.target_chain_id,
        target_token: fx.target_token,
        adapter_program: fx.adapter_program,
        accounts: second_accounts,
        owner_ata: second_owner_ata,
        across_state: second_across_state,
        across_vault: second_across_vault,
    };

    let meta_b0 = second_fx
        .invoke(&mut svm, params.clone())
        .expect("second mint deposit #0 should succeed");
    let ev_b0 = extract_deposit_prepared(&meta_b0.logs).expect("second mint event #0");

    let meta_a1 = fx
        .invoke(&mut svm, params)
        .expect("first mint deposit #1 should succeed");
    let ev_a1 = extract_deposit_prepared(&meta_a1.logs).expect("first mint event #1");

    assert_eq!(ev_a0.sra_config, fx.sra_config);
    assert_eq!(ev_b0.sra_config, fx.sra_config);
    assert_eq!(ev_a0.source_mint, fx.source_mint);
    assert_eq!(ev_b0.source_mint, second_mint);
    assert_eq!(ev_a0.nonce, 0);
    assert_eq!(ev_b0.nonce, 0);
    assert_eq!(ev_a1.nonce, 1);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 2);
    assert_eq!(
        fetch_mint_nonce(&svm, &second_fx.accounts.mint_nonce).nonce,
        1
    );
}

#[test]
fn validate_and_prepare_route_mismatch_rejected() {
    // EVM RouteConfig parity: routing is committed at install (folded into
    // the SRA identity via handler_data_hash). A deposit whose params diverge from
    // the committed route on ANY field must be rejected before funds move — a
    // compromised tx-builder must not be able to redirect a deposit the way it
    // could on a Solana port that left routing as free per-deposit input.
    type RouteMutator = fn(&mut PrepareParams);
    let cases: [(&str, RouteMutator); 3] = [
        ("target_chain_id", |params| params.target_chain_id = 10),
        ("recipient", |params| params.recipient = evm_bytes32(0x34)),
        ("target_token", |params| {
            params.target_token = evm_bytes32(0xBC)
        }),
    ];

    for (name, mutate) in cases {
        let mut svm = setup_svm();
        let fx = ValidatePrepareFixture::new(&mut svm);
        let mut params = default_prepare_params(&fx);
        mutate(&mut params);
        // Ensure an enabled address-book entry exists for whatever (chain, token)
        // the mutated params point at, so the rejection is attributable to the
        // route-commitment check rather than a missing/disabled book entry.
        setup_address_book_entry(
            &mut svm,
            &fx.payer,
            &fx.owner,
            &fx.address_book_root,
            &fx.source_mint,
            params.target_chain_id,
            params.target_token,
            6,
        );

        let err = fx
            .invoke(&mut svm, params)
            .expect_err(&format!("{name} mismatch must reject"))
            .err
            .clone();

        assert_eq!(
            err,
            TransactionError::InstructionError(
                0,
                InstructionError::Custom(SraError::RouteMismatch.into()),
            ),
            "{name} mismatch must reject with RouteMismatch",
        );
        assert_eq!(
            fetch_token_balance(&svm, &fx.owner_ata),
            OWNER_STARTING_BALANCE,
            "{name} mismatch must leave owner funds untouched",
        );
    }
}

#[test]
fn validate_and_prepare_rejection_preserves_nonce() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let meta_0 = fx
        .invoke(&mut svm, params.clone())
        .expect("initial deposit must succeed");
    let ev_0 = extract_deposit_prepared(&meta_0.logs).expect("missing DepositPrepared #0");
    assert_eq!(ev_0.nonce, 0);

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        0,
    );
    let err = fx
        .invoke(&mut svm, params.clone())
        .expect_err("address-book tombstone must reject")
        .err
        .clone();
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAddressBook.into()),
        )
    );
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 1);

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        6,
    );
    let meta_1 = fx
        .invoke(&mut svm, params.clone())
        .expect("post-enable deposit must succeed");
    let ev_1 = extract_deposit_prepared(&meta_1.logs).expect("missing DepositPrepared #1");
    assert_eq!(ev_1.nonce, 1);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 2);

    let mut wrong_handler_data = fx.handler_data.clone();
    wrong_handler_data.allowed_adapters.clear();
    let err = fx
        .invoke_with_handler_data(&mut svm, wrong_handler_data, params.clone(), vec![])
        .expect_err("altered handler_data must reject")
        .err
        .clone();
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidHandlerData.into()),
        )
    );
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 2);

    let meta_2 = fx
        .invoke(&mut svm, params)
        .expect("post-handler-data-fix deposit must succeed");
    let ev_2 = extract_deposit_prepared(&meta_2.logs).expect("missing DepositPrepared #2");
    assert_eq!(ev_2.nonce, 2);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 3);
}

#[test]
fn validate_and_prepare_zero_input_amount_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: 0,
                output_amount: u256_from_u64(0),
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("zero input amount must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::ZeroInputAmount.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
    assert_eq!(current_nonce(&svm, &fx.accounts.mint_nonce), 0);
}

#[test]
fn validate_and_prepare_token_2022_mint_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    restamp_mint_as_token_2022(&mut svm, &fx.source_mint);

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("token-2022 source mint must reject by construction")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(
                anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram as u32,
            ),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
    assert_eq!(current_nonce(&svm, &fx.accounts.mint_nonce), 0);
}

#[test]
fn validate_and_prepare_frozen_owner_ata_fails_cleanly() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    freeze_token_account(&mut svm, &fx.owner_ata);

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("frozen owner ATA must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(
                litesvm_token::spl_token::error::TokenError::AccountFrozen as u32,
            ),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
    assert_eq!(current_nonce(&svm, &fx.accounts.mint_nonce), 0);
}

#[test]
fn output_above_u64_max_passes_handler_but_cctp_rejects_quote() {
    // The handler widens the u256 output and accepts it (the IX-1 error index
    // proves IX 0 passed); the CCTP adapter then rejects it — a u64 net can
    // never cover an output above u64::MAX, by the quote invariant.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let mut output_amount = [0u8; 32];
    output_amount[16..].copy_from_slice(&(1u128 << 64).to_be_bytes());

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                output_amount,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("output above u64::MAX must reject in the adapter")
        .err
        .clone();

    adapter_error_at_1(
        err,
        CctpAdapterError::QuoteExceedsNetAfterFee,
        "post-widening handler acceptance, CCTP quote rejection",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "rejection reverts the whole paired tx",
    );
}

#[test]
fn validate_and_prepare_output_amount_overflow_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let mut output_amount = [0u8; 32];
    output_amount[15] = 1;

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                output_amount,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("output_amount above the u128 max must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::OutputAmountOverflow.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
    assert_eq!(current_nonce(&svm, &fx.accounts.mint_nonce), 0);
}
