mod common;

use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::{constants::MAX_ACTION_IDXS, errors::SraError, source_deposit::PrepareParams};

const INPUT: u64 = 1_000_000;
// 50 bps slippage tolerance -> min acceptable = 995_000 for INPUT = 1_000_000.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const DEFAULT_ACTION_IDXS: [u64; 2] = [3, 7];

fn default_prepare_params(fx: &ValidatePrepareFixture) -> PrepareParams {
    PrepareParams {
        input_amount: INPUT,
        output_amount: OUTPUT_WITHIN_SLIPPAGE,
        target_token: fx.target_token,
        action_idxs: DEFAULT_ACTION_IDXS.to_vec(),
    }
}

#[test]
fn validate_and_prepare_happy_path() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("validate_and_prepare should succeed");

    // Tokens flowed owner -> SRA vault -> adapter custody -> Across vault in a single tx.
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
        fetch_token_balance(&svm, &fx.across_vault),
        INPUT,
        "Across vault receives the bridged tokens",
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
    assert_eq!(ev.output_amount, OUTPUT_WITHIN_SLIPPAGE);
    assert_eq!(ev.target_chain_id, fx.config.target_chain_id);
    assert_eq!(ev.target_handler, fx.config.target_handler);
    assert_eq!(ev.target_adapter, fx.config.target_adapter);
    assert_eq!(ev.recipient, fx.config.recipient);
    assert_eq!(ev.app_id, fx.config.app_id);
    assert_eq!(ev.target_token, params.target_token);
    assert_eq!(ev.action_idxs, params.action_idxs);
}

#[test]
fn validate_and_prepare_standalone_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        default_prepare_params(&fx),
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

    let mut bad_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &params,
        across_params_for(&fx, &params),
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );
    bad_ix.program_id = PROGRAM_ID;

    let address_book_entry = fx.address_book_entry(&params.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        params,
    );
    let err = send_instructions(&mut svm, &[validate_ix, bad_ix], &fx.payer, &[&fx.owner])
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
    let mut across = across_params_for(&fx, &prepare);
    across.action_idxs[1] += 1;

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );
    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
    )
    .expect_err("routing mismatch must reject")
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
fn validate_and_prepare_wrong_next_ix_sra_config_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    let across = across_params_for(&fx, &prepare);

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let mut execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );
    // ADAPTER_ACCT_SRA_CONFIG = 2 in the adapter ix layout.
    execute_ix.accounts[2].pubkey = solana_sdk::pubkey::Pubkey::new_unique();

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
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
    let across = across_params_for(&fx, &prepare);

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let mut execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );
    // ADAPTER_ACCT_INPUT_MINT = 6 in the adapter ix layout.
    execute_ix.accounts[6].pubkey = solana_sdk::pubkey::Pubkey::new_unique();

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
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
fn validate_and_prepare_wrong_next_ix_target_token_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    let mut across = across_params_for(&fx, &prepare);
    // Differ only in target_token. Other normalized fields stay aligned so the
    // handler verifier rejects on routing data, not on account slots.
    across.target_token[31] ^= 0xFF;

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
    )
    .expect_err("target_token mismatch in IX 2 must reject")
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
fn validate_and_prepare_wrong_next_ix_input_amount_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    let mut across = across_params_for(&fx, &prepare);
    across.input_amount += 1;

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
    )
    .expect_err("input_amount mismatch in IX 2 must reject")
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
    let across = across_params_for(&fx, &prepare);

    let address_book_entry = fx.address_book_entry(&fx.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        prepare.clone(),
    );
    let mut execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across,
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );
    // Wrong discriminator: zero out the leading 8 bytes so decode rejects
    // before any field comparison.
    for byte in execute_ix.data.iter_mut().take(8) {
        *byte = 0;
    }

    let err = send_instructions(
        &mut svm,
        &[validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
    )
    .expect_err("malformed IX 2 data must reject")
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
    let err = invoke_execute_only(&fx, &mut svm, &prepare, across_params_for(&fx, &prepare))
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
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across_params_for(&fx, &prepare),
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );

    let err = send_instructions(&mut svm, &[compute_budget_ix(), execute_ix], &fx.payer, &[])
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
    // Use a real but different sra_handler instruction (`update_adapters`) as the
    // preceding ix. The adapter verifier passes the program_id check but rejects
    // on the account-count guard since update_adapters has fewer accounts than
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
    let other_handler_ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![ADAPTER_PROGRAM_ID],
    );
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &prepare,
        across_params_for(&fx, &prepare),
        current_nonce(&svm, &fx.accounts.mint_nonce),
    );

    let err = send_instructions(
        &mut svm,
        &[other_handler_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
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
fn validate_and_prepare_disabled_address_book_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.sra_config,
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
    let fx = ValidatePrepareFixture::new(&mut svm);

    let ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![],
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("clear adapters");

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
    let mut fx = ValidatePrepareFixture::new(&mut svm);
    let non_executable_adapter = funded_keypair(&mut svm, SOL);

    let ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![non_executable_adapter.pubkey()],
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("update adapters");

    fx.adapter_program = non_executable_adapter.pubkey();
    fx.accounts =
        ValidatePrepareAccounts::derive(&fx.sra_config, &fx.source_mint, &fx.adapter_program);

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
fn validate_and_prepare_slippage_exceeded_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                output_amount: 900_000,
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
fn validate_and_prepare_too_many_action_idxs_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let too_many = (0..=MAX_ACTION_IDXS as u64).collect::<Vec<_>>();
    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                action_idxs: too_many,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("too many action idxs must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::TooManyActionIdxs.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
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
                output_amount: over_balance,
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

    let params = PrepareParams {
        input_amount: 100_000,
        output_amount: 99_500,
        target_token: fx.target_token,
        action_idxs: DEFAULT_ACTION_IDXS.to_vec(),
    };

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
        assert_eq!(ev.action_idxs, params.action_idxs);

        let n = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
        assert_eq!(
            n.nonce,
            expected_pre_nonce + 1,
            "nonce advances after each deposit",
        );
    }

    // After 3 successful deposits: across_vault holds 3x 100_000, owner is short the same.
    assert_eq!(fetch_token_balance(&svm, &fx.across_vault), 300_000);
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - 300_000
    );
}
