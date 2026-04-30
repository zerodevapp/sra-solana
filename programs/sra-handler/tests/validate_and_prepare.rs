mod common;

use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::TransactionError,
};
use sra_common::{
    abi::u256_from_u64,
    constants::MAX_ACTION_IDXS,
    errors::SraError,
    source_deposit::{AcrossDepositParams, PrepareParams},
};

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

fn default_across_params(payer: &Keypair, params: &PrepareParams) -> AcrossDepositParams {
    AcrossDepositParams {
        input_amount: params.input_amount,
        output_amount: sra_common::source_deposit::handler_output_amount_u256(params.output_amount),
        target_token: params.target_token,
        action_idxs: params.action_idxs.clone(),
        exclusive_relayer: payer.pubkey(),
        fill_deadline_offset: 3_600,
        exclusivity_period: 900,
    }
}

fn build_deposit_bundle(
    fx: &ValidatePrepareFixture,
    svm: &LiteSVM,
    payer: &Keypair,
    params: &PrepareParams,
    prepared_nonce: u64,
) -> sra_across_adapter::utils::DepositNowBuild {
    sra_across_adapter::utils::build_deposit_now_bundle(
        &fx.accounts.adapter_authority,
        &fetch_sra_config(svm, &fx.sra_config),
        &fx.source_mint,
        params,
        &default_across_params(payer, params),
        prepared_nonce,
    )
    .expect("build deposit-now bundle")
}

fn shell_fixture(
    config: sra_handler::instructions::source::initialize_sra::SraInitConfig,
    sra_config: Pubkey,
    source_mint: Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    adapter_program: Pubkey,
    accounts: ValidatePrepareAccounts,
    owner_ata: Pubkey,
    across_state: Pubkey,
    across_vault: Pubkey,
) -> ValidatePrepareFixture {
    ValidatePrepareFixture {
        payer: Keypair::new(),
        owner: Keypair::new(),
        config,
        sra_config,
        source_mint,
        target_chain_id,
        target_token,
        adapter_program,
        accounts,
        owner_ata,
        across_state,
        across_vault,
    }
}

fn invoke_source_flow(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    fx: &ValidatePrepareFixture,
    owner_ata: &Pubkey,
    params: PrepareParams,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    let address_book_entry =
        derive_address_book_pda(&fx.sra_config, fx.target_chain_id, &params.target_token).0;
    let validate_ix = validate_and_prepare_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &fx.accounts,
        owner_ata,
        &address_book_entry,
        params.clone(),
    );
    let execute_ix = execute_deposit_across_ix(
        &payer.pubkey(),
        fx,
        &fetch_sra_config(svm, &fx.sra_config),
        &params,
        default_across_params(payer, &params),
        current_nonce(svm, &fx.accounts.mint_nonce),
    );
    send_instructions(svm, &[validate_ix, execute_ix], payer, &[owner])
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
fn execute_deposit_emits_across_deposit_executed() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("paired source flow should succeed");

    let ev = extract_anchor_event::<sra_across_adapter::events::AcrossDepositExecuted>(&meta.logs)
        .expect("AcrossDepositExecuted event must be emitted");
    assert_eq!(ev.sra_config, fx.sra_config);
    assert_eq!(ev.input_mint, fx.source_mint);
    assert_eq!(ev.nonce, 0);
    assert_eq!(ev.across_state, fx.across_state);
    assert_eq!(ev.across_vault, fx.across_vault);
    assert_eq!(ev.input_amount, params.input_amount);
    assert_eq!(
        ev.output_amount,
        sra_common::source_deposit::handler_output_amount_u256(params.output_amount)
    );
    assert_eq!(ev.target_token, params.target_token);
    assert_eq!(ev.action_idxs, params.action_idxs);
    assert_eq!(ev.exclusive_relayer, fx.payer.pubkey());
    assert_eq!(ev.fill_deadline_offset, 3_600);
    assert_eq!(ev.exclusivity_period, 900);
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
fn validate_and_prepare_wrong_next_ix_output_amount_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let prepare = default_prepare_params(&fx);
    let mut across = across_params_for(&fx, &prepare);
    across.output_amount =
        sra_common::source_deposit::handler_output_amount_u256(prepare.output_amount + 1);

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
    .expect_err("output_amount mismatch in IX 2 must reject")
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

#[test]
fn validate_and_prepare_slippage_boundary_succeeds() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let min_acceptable = 995_000;
    let params = PrepareParams {
        output_amount: min_acceptable,
        ..default_prepare_params(&fx)
    };

    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("boundary slippage output must succeed");

    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event");
    assert_eq!(ev.output_amount, min_acceptable);
    assert_eq!(ev.nonce, 0);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 1);
}

#[test]
fn validate_and_prepare_max_action_idxs_happy_path() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let action_idxs = vec![0, 1, 2, u64::MAX];
    let params = PrepareParams {
        action_idxs: action_idxs.clone(),
        ..default_prepare_params(&fx)
    };

    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("MAX_ACTION_IDXS deposit must succeed");

    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event");
    assert_eq!(ev.action_idxs, action_idxs);

    let build = build_deposit_bundle(&fx, &svm, &fx.payer, &params, ev.nonce);
    assert_eq!(
        build.message.action_idxs,
        action_idxs
            .iter()
            .copied()
            .map(u256_from_u64)
            .collect::<Vec<_>>()
    );
}

#[test]
fn validate_and_prepare_same_sra_mint_distinct_messages_and_replay_hashes() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let meta_0 = fx
        .invoke(&mut svm, params.clone())
        .expect("first deposit should succeed");
    let ev_0 = extract_deposit_prepared(&meta_0.logs).expect("first DepositPrepared event");
    let build_0 = build_deposit_bundle(&fx, &svm, &fx.payer, &params, ev_0.nonce);

    let meta_1 = fx
        .invoke(&mut svm, params.clone())
        .expect("second deposit should succeed");
    let ev_1 = extract_deposit_prepared(&meta_1.logs).expect("second DepositPrepared event");
    let build_1 = build_deposit_bundle(&fx, &svm, &fx.payer, &params, ev_1.nonce);

    assert_eq!(ev_0.nonce, 0);
    assert_eq!(ev_1.nonce, 1);
    assert_ne!(build_0.params.message, build_1.params.message);
    assert_ne!(build_0.replay_hash, build_1.replay_hash);
    assert_ne!(
        build_0.message.deposit_info.extra,
        build_1.message.deposit_info.extra
    );
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
    let second_fx = shell_fixture(
        fx.config.clone(),
        fx.sra_config,
        second_mint,
        fx.target_chain_id,
        fx.target_token,
        fx.adapter_program,
        second_accounts,
        second_owner_ata,
        second_across_state,
        second_across_vault,
    );

    let meta_b0 = invoke_source_flow(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &second_fx,
        &second_owner_ata,
        params.clone(),
    )
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
fn validate_and_prepare_same_owner_two_sras_same_mint_independent_nonces() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let mut second_config = fx.config.clone();
    second_config.app_id[0] ^= 0xFF;
    let second_salt = [0xCDu8; 32];
    let (second_sra_config, _) = derive_sra_pda(&fx.owner.pubkey(), &second_config, &second_salt);
    let init_ix = initialize_sra_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &second_sra_config,
        second_config.clone(),
        second_salt,
    );
    send_ix(&mut svm, init_ix, &fx.payer, &[&fx.owner]).expect("initialize second sra");
    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &second_sra_config,
        fx.target_chain_id,
        fx.target_token,
        6,
    );
    let set_adapters_ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &second_sra_config,
        vec![fx.adapter_program],
    );
    send_ix(&mut svm, set_adapters_ix, &fx.payer, &[&fx.owner]).expect("set second adapters");
    let set_slippage_ix = update_slippage_ix(&fx.owner.pubkey(), &second_sra_config, 50);
    send_ix(&mut svm, set_slippage_ix, &fx.payer, &[&fx.owner]).expect("set second slippage");

    let second_accounts =
        ValidatePrepareAccounts::derive(&second_sra_config, &fx.source_mint, &fx.adapter_program);
    let second_across_state = funded_keypair(&mut svm, SOL).pubkey();
    let second_across_vault =
        create_ata(&mut svm, &fx.payer, &second_across_state, &fx.source_mint);
    let second_fx = shell_fixture(
        second_config,
        second_sra_config,
        fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        fx.adapter_program,
        second_accounts,
        fx.owner_ata,
        second_across_state,
        second_across_vault,
    );

    let meta_a0 = fx
        .invoke(&mut svm, params.clone())
        .expect("first sra deposit #0 should succeed");
    let ev_a0 = extract_deposit_prepared(&meta_a0.logs).expect("first sra event #0");

    let meta_b0 = invoke_source_flow(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &second_fx,
        &fx.owner_ata,
        params.clone(),
    )
    .expect("second sra deposit #0 should succeed");
    let ev_b0 = extract_deposit_prepared(&meta_b0.logs).expect("second sra event #0");

    let meta_a1 = fx
        .invoke(&mut svm, params)
        .expect("first sra deposit #1 should succeed");
    let ev_a1 = extract_deposit_prepared(&meta_a1.logs).expect("first sra event #1");

    assert_ne!(fx.sra_config, second_fx.sra_config);
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
fn validate_and_prepare_tombstone_and_reenable_route_preserves_nonce() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let meta_0 = fx
        .invoke(&mut svm, params.clone())
        .expect("deposit before tombstone should succeed");
    let ev_0 = extract_deposit_prepared(&meta_0.logs).expect("DepositPrepared event #0");
    assert_eq!(ev_0.nonce, 0);

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
        .invoke(&mut svm, params.clone())
        .expect_err("tombstoned route must reject")
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
        &fx.sra_config,
        fx.target_chain_id,
        fx.target_token,
        6,
    );
    let meta_1 = fx
        .invoke(&mut svm, params)
        .expect("re-enabled route should succeed");
    let ev_1 = extract_deposit_prepared(&meta_1.logs).expect("DepositPrepared event #1");
    assert_eq!(ev_1.nonce, 1);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 2);
}

#[test]
fn validate_and_prepare_adapter_clear_and_restore_preserves_nonce() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let params = default_prepare_params(&fx);

    let meta_0 = fx
        .invoke(&mut svm, params.clone())
        .expect("deposit before adapter clear should succeed");
    let ev_0 = extract_deposit_prepared(&meta_0.logs).expect("DepositPrepared event #0");
    assert_eq!(ev_0.nonce, 0);

    let clear_ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![],
    );
    send_ix(&mut svm, clear_ix, &fx.payer, &[&fx.owner]).expect("clear adapters");
    let err = fx
        .invoke(&mut svm, params.clone())
        .expect_err("cleared adapter list must reject")
        .err
        .clone();
    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::AdapterNotAllowed.into()),
        )
    );
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 1);

    let restore_ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![ADAPTER_PROGRAM_ID],
    );
    send_ix(&mut svm, restore_ix, &fx.payer, &[&fx.owner]).expect("restore adapters");
    let meta_1 = fx
        .invoke(&mut svm, params)
        .expect("restored adapter list should succeed");
    let ev_1 = extract_deposit_prepared(&meta_1.logs).expect("DepositPrepared event #1");
    assert_eq!(ev_1.nonce, 1);
    assert_eq!(fetch_mint_nonce(&svm, &fx.accounts.mint_nonce).nonce, 2);
}
