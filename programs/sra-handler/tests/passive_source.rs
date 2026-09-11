mod common;

use anchor_lang::{error::ErrorCode, AccountSerialize, InstructionData, ToAccountMetas};
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    message::{Message, VersionedMessage},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{TransactionError, VersionedTransaction},
};
use sra_across_adapter::AcrossAdapterError;
use sra_cctp_adapter::CctpAdapterError;
use sra_common::{
    abi::u256_from_u64,
    constants::FINALITY_THRESHOLD_FAST,
    errors::SraError,
    source_deposit::{AcrossDepositParams, CctpDepositParams, PrepareParams},
    types::SraConfig,
};

struct SourceFixture {
    payer: Keypair,
    owner: Keypair,
    initiator: Keypair,
    sra: Pubkey,
    source_mint: Pubkey,
}

impl SourceFixture {
    fn new(svm: &mut LiteSVM) -> Self {
        let payer = funded_keypair(svm, 10 * SOL);
        let owner = funded_keypair(svm, SOL);
        let initiator = funded_keypair(svm, SOL);
        let address_book_root = init_address_book_root(svm, &payer, &owner);
        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        let handler_data = sample_handler_data(address_book_root);
        let salt = [0xB3; 32];
        let (sra, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
        let ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root,
            &sra,
            handler_data,
            salt,
            initiator.pubkey(),
            source_mint,
        );
        send_ix(svm, ix, &payer, &[&owner]).unwrap();
        Self {
            payer,
            owner,
            initiator,
            sra,
            source_mint,
        }
    }
}

fn manage_sra_ix(owner: Pubkey, sra: Pubkey, initiator: Pubkey, active: bool) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ManageSra {
            owner,
            sra_config: sra,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ManageSra { initiator, active }.data(),
    }
}

fn manage_sra(
    svm: &mut LiteSVM,
    fx: &SourceFixture,
    initiator: Pubkey,
    active: bool,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    send_ix_with_meta(
        svm,
        manage_sra_ix(fx.owner.pubkey(), fx.sra, initiator, active),
        &fx.payer,
        &[&fx.owner],
    )
}

#[test]
fn manage_sra_replaces_only_runtime_control() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let replacement = Keypair::new();
    let vault = derive_ata(&fx.sra, &fx.source_mint);
    mint_to(
        &mut svm,
        &fx.payer,
        &fx.payer,
        &fx.source_mint,
        &vault,
        1_000_000,
    );
    let vault_before = svm.get_account(&vault).unwrap();
    let mut state = fetch_sra_config(&svm, &fx.sra);
    state.deposit_nonce = u64::MAX;
    let mut account = svm.get_account(&fx.sra).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(fx.sra, account).unwrap();
    let immutable_before = (
        state.owner,
        state.final_handler,
        state.handler_data.clone(),
        state.salt,
        state.deposit_nonce,
        state.bump,
    );
    let meta = manage_sra(&mut svm, &fx, replacement.pubkey(), false).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra);
    assert_eq!(state.initiator, replacement.pubkey());
    assert!(!state.active);
    assert_eq!(
        (
            state.owner,
            state.final_handler,
            state.handler_data,
            state.salt,
            state.deposit_nonce,
            state.bump,
        ),
        immutable_before,
    );
    assert_eq!(svm.get_account(&vault).unwrap(), vault_before);
    let event = extract_anchor_event::<sra_handler::events::SraManaged>(&meta.logs)
        .expect("SraManaged event must be emitted");
    assert_eq!(event.sra_config, fx.sra);
    assert_eq!(event.owner, fx.owner.pubkey());
    assert_eq!(event.initiator, replacement.pubkey());
    assert!(!event.active);
    assert_eq!(event.deposit_nonce, u64::MAX);
}

#[test]
fn manage_sra_non_owner_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let attacker = funded_keypair(&mut svm, SOL);
    let before = svm.get_account(&fx.sra).unwrap().data;
    for signer in [&fx.initiator, &attacker] {
        let err = send_ix_expect_err(
            &mut svm,
            manage_sra_ix(signer.pubkey(), fx.sra, attacker.pubkey(), false),
            &fx.payer,
            &[signer],
            "only the owner may manage an SRA",
        );
        assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
        assert_eq!(err, anchor_error(ErrorCode::ConstraintHasOne));
    }
}

#[test]
fn manage_sra_zero_initiator_rejected_without_changes() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra).unwrap().data;
    let err = manage_sra(&mut svm, &fx, Pubkey::default(), false).unwrap_err();
    assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
    assert_eq!(err.err, custom_error(SraError::InvalidInitiator));
}

#[test]
fn manage_sra_requires_owner_signature() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    let before = svm.get_account(&fx.sra).unwrap().data;
    let mut ix = manage_sra_ix(fx.owner.pubkey(), fx.sra, fx.initiator.pubkey(), false);
    ix.accounts[0].is_signer = false;
    let err = send_ix_expect_err(&mut svm, ix, &fx.payer, &[], "owner must sign management");
    assert_eq!(svm.get_account(&fx.sra).unwrap().data, before);
    assert_eq!(err, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn manage_sra_owner_can_reactivate() {
    let mut svm = setup_svm();
    let fx = SourceFixture::new(&mut svm);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), false).unwrap();
    assert!(!fetch_sra_config(&svm, &fx.sra).active);
    manage_sra(&mut svm, &fx, fx.initiator.pubkey(), true).unwrap();
    let state = fetch_sra_config(&svm, &fx.sra);
    assert!(state.active);
    assert_eq!(state.initiator, fx.initiator.pubkey());
    assert_eq!(state.deposit_nonce, 0);
}

fn set_source_balance(svm: &mut LiteSVM, fx: &common::PassiveFixture, amount: u64) {
    let mut vault_account = svm.get_account(&fx.accounts.sra_vault).unwrap();
    let mut vault = litesvm_token::spl_token::state::Account::unpack(&vault_account.data).unwrap();
    vault.amount = amount;
    vault.pack_into_slice(&mut vault_account.data);
    svm.set_account(fx.accounts.sra_vault, vault_account)
        .unwrap();
    let mut mint_account = svm.get_account(&fx.source_mint).unwrap();
    let mut mint = litesvm_token::spl_token::state::Mint::unpack(&mint_account.data).unwrap();
    mint.supply = amount;
    mint.pack_into_slice(&mut mint_account.data);
    svm.set_account(fx.source_mint, mint_account).unwrap();
}

#[test]
fn passive_cctp_happy_path() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 3_000;
    params.output_amount = u256_from_u64(995_000);
    let (execute_ix, event_data) = execute_deposit_cctp_ix(
        &fx.payer.pubkey(),
        &fx,
        &CctpDepositParams::from_prepare(&params, 1_000, FINALITY_THRESHOLD_FAST).unwrap(),
    );
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
        Some(fee_ata),
        params.clone(),
        &execute_ix,
    );
    svm.expire_blockhash();
    let tx = VersionedTransaction::try_new(
        VersionedMessage::Legacy(Message::new_with_blockhash(
            &[trigger, execute_ix],
            Some(&fx.payer.pubkey()),
            &svm.latest_blockhash(),
        )),
        &[&fx.payer, &fx.initiator, &event_data],
    )
    .unwrap();
    let owner_index = tx
        .message
        .static_account_keys()
        .iter()
        .position(|key| key == &fx.owner.pubkey());
    assert!(owner_index.is_none_or(|index| !tx.message.is_signer(index)));
    assert_eq!(tx.signatures.len(), 3);
    let fee_before = fetch_token_balance(&svm, &fee_ata);
    let metadata = svm.send_transaction(tx).unwrap();
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), fee_before + 3_000);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.adapter_custody), 0);
    assert_eq!(
        fetch_sent_deposit_for_burn(&svm, &event_data.pubkey()).amount,
        997_000
    );
    assert_eq!(fetch_sra_config(&svm, &fx.sra_config).deposit_nonce, 1);
    let triggered = extract_deposit_triggered(&metadata.logs).unwrap();
    assert_eq!(triggered.sra_config, fx.sra_config);
    assert_eq!(triggered.initiator, fx.initiator.pubkey());
    assert_eq!(triggered.source_mint, fx.source_mint);
    assert_eq!(triggered.adapter_program, CCTP_ADAPTER_PROGRAM_ID);
    assert_eq!(triggered.nonce, 0);
    assert_eq!(triggered.input_amount, 1_000_000);
    assert_eq!(triggered.protocol_fee_amount, 3_000);
    assert_eq!(triggered.output_amount, u256_from_u64(995_000));
    assert_eq!(triggered.target_chain_id, params.target_chain_id);
    assert_eq!(triggered.recipient, params.recipient);
    assert_eq!(triggered.target_token, params.target_token);
    let executed =
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap();
    assert_eq!(executed.nonce, 0);
}

#[test]
fn passive_across_happy_path() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new_with_adapter_policy(
        &mut svm,
        ADAPTER_PROGRAM_ID,
        50,
        vec![ADAPTER_PROGRAM_ID],
    );
    set_source_balance(&mut svm, &fx, 2_000_000);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 3_000;
    params.output_amount = u256_from_u64(995_000);
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
        &fetch_sra_config(&svm, &fx.sra_config),
        &params,
        AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120).unwrap(),
    );
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
        Some(fee_ata),
        params.clone(),
        &execute_ix,
    );
    svm.expire_blockhash();
    let tx = VersionedTransaction::try_new(
        VersionedMessage::Legacy(Message::new_with_blockhash(
            &[trigger, execute_ix],
            Some(&fx.payer.pubkey()),
            &svm.latest_blockhash(),
        )),
        &[&fx.payer, &fx.initiator],
    )
    .unwrap();
    let owner_index = tx
        .message
        .static_account_keys()
        .iter()
        .position(|key| key == &fx.owner.pubkey());
    assert!(owner_index.is_none_or(|index| !tx.message.is_signer(index)));
    assert_eq!(tx.signatures.len(), 2);
    let fee_before = fetch_token_balance(&svm, &fee_ata);
    let across_before = fetch_token_balance(&svm, &fx.across_vault);
    let metadata = svm.send_transaction(tx).unwrap();
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), fee_before + 3_000);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.adapter_custody), 0);
    assert_eq!(
        fetch_token_balance(&svm, &fx.across_vault),
        across_before + 997_000
    );
    assert_eq!(fetch_sra_config(&svm, &fx.sra_config).deposit_nonce, 1);
    let triggered = extract_deposit_triggered(&metadata.logs).unwrap();
    assert_eq!(triggered.initiator, fx.initiator.pubkey());
    assert_eq!(triggered.nonce, 0);
    assert_eq!(triggered.protocol_fee_amount, 3_000);
    assert_eq!(triggered.target_chain_id, params.target_chain_id);
    assert_eq!(triggered.recipient, params.recipient);
    assert_eq!(triggered.target_token, params.target_token);
    let executed =
        extract_anchor_event::<sra_across_adapter::events::AcrossDepositExecuted>(&metadata.logs)
            .unwrap();
    assert_eq!(executed.nonce, 0);
}

#[test]
fn passive_sra_nonce_progresses_across_adapters_and_mints() {
    let mut svm = setup_svm();
    let cctp = common::PassiveFixture::new_with_adapter_policy(
        &mut svm,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID],
    );
    let cctp_params = sample_prepare_params(cctp.target_token);
    let (trigger, execute, event) = cctp_pair(
        &cctp,
        cctp_params.clone(),
        cctp_params_for(&cctp_params),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &cctp.payer,
        &[&cctp.initiator, &event],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &cctp.sra_config).deposit_nonce, 1);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 0);
    assert_eq!(
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        0
    );

    let across_mint = create_mint(&mut svm, &cctp.payer, &cctp.payer.pubkey(), 6);
    let across_sra_vault = create_ata(&mut svm, &cctp.payer, &cctp.sra_config, &across_mint);
    mint_to(
        &mut svm,
        &cctp.payer,
        &cctp.payer,
        &across_mint,
        &across_sra_vault,
        SRA_STARTING_BALANCE,
    );
    setup_address_book_entry(
        &mut svm,
        &cctp.payer,
        &cctp.owner,
        &cctp.address_book_root,
        &across_mint,
        cctp.target_chain_id,
        cctp.target_token,
        6,
    );
    let across_accounts =
        TriggerAccounts::derive(&cctp.sra_config, &across_mint, &ADAPTER_PROGRAM_ID);
    let across_vault = create_ata(&mut svm, &cctp.payer, &cctp.across_state, &across_mint);
    let across = common::PassiveFixture {
        payer: cctp.payer.insecure_clone(),
        owner: cctp.owner.insecure_clone(),
        initiator: cctp.initiator.insecure_clone(),
        fee_admin: cctp.fee_admin.insecure_clone(),
        address_book_root: cctp.address_book_root,
        handler_data: cctp.handler_data.clone(),
        sra_config: cctp.sra_config,
        source_mint: across_mint,
        target_chain_id: cctp.target_chain_id,
        target_token: cctp.target_token,
        adapter_program: ADAPTER_PROGRAM_ID,
        accounts: across_accounts,
        owner_ata: create_ata(&mut svm, &cctp.payer, &cctp.owner.pubkey(), &across_mint),
        across_state: cctp.across_state,
        across_vault,
    };
    let across_params = sample_prepare_params(across.target_token);
    let (trigger, execute) = across_pair(
        &svm,
        &across,
        across_params.clone(),
        AcrossDepositParams::from_prepare(&across_params, Pubkey::new_unique(), 600, 120).unwrap(),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &across.payer,
        &[&across.initiator],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &across.sra_config).deposit_nonce, 2);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 1);
    assert_eq!(
        extract_anchor_event::<sra_across_adapter::events::AcrossDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        1
    );

    let (trigger, execute, event) = cctp_pair(
        &cctp,
        cctp_params.clone(),
        cctp_params_for(&cctp_params),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &cctp.payer,
        &[&cctp.initiator, &event],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &cctp.sra_config).deposit_nonce, 3);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 2);
    assert_eq!(
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        2
    );
}

#[derive(Debug, PartialEq, Eq)]
struct PassiveSnapshot {
    sra_vault_balance: u64,
    fee_recipient_balance: u64,
    adapter_custody_balance: u64,
    source_mint_supply: u64,
    sra_config: Vec<u8>,
    across_vault_balance: u64,
    circle_event_data: Option<Vec<u8>>,
}

fn token_balance_or_zero(svm: &LiteSVM, address: &Pubkey) -> u64 {
    svm.get_account(address)
        .map(|_| fetch_token_balance(svm, address))
        .unwrap_or(0)
}

fn passive_snapshot(
    svm: &LiteSVM,
    fx: &common::PassiveFixture,
    circle_event: Option<Pubkey>,
) -> PassiveSnapshot {
    let fee_recipient = fetch_fee_config(svm, &derive_fee_config_pda().0).fee_recipient;
    PassiveSnapshot {
        sra_vault_balance: fetch_token_balance(svm, &fx.accounts.sra_vault),
        fee_recipient_balance: token_balance_or_zero(
            svm,
            &derive_ata(&fee_recipient, &fx.source_mint),
        ),
        adapter_custody_balance: token_balance_or_zero(svm, &fx.accounts.adapter_custody),
        source_mint_supply: fetch_mint_supply(svm, &fx.source_mint),
        sra_config: svm.get_account(&fx.sra_config).unwrap().data,
        across_vault_balance: fetch_token_balance(svm, &fx.across_vault),
        circle_event_data: circle_event
            .and_then(|address| svm.get_account(&address))
            .map(|account| account.data),
    }
}

fn set_sra_config(svm: &mut LiteSVM, sra: &Pubkey, mutate: impl FnOnce(&mut SraConfig)) {
    let mut state = fetch_sra_config(svm, sra);
    mutate(&mut state);
    let mut account = svm.get_account(sra).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(*sra, account).unwrap();
}

fn cctp_pair(
    fx: &common::PassiveFixture,
    params: PrepareParams,
    cctp: CctpDepositParams,
    fee_ata: Option<Pubkey>,
) -> (Instruction, Instruction, Keypair) {
    let (execute, event) = execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp);
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        fee_ata,
        params,
        &execute,
    );
    (trigger, execute, event)
}

fn across_pair(
    svm: &LiteSVM,
    fx: &common::PassiveFixture,
    params: PrepareParams,
    across: AcrossDepositParams,
    fee_ata: Option<Pubkey>,
) -> (Instruction, Instruction) {
    let execute = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        fx,
        &fetch_sra_config(svm, &fx.sra_config),
        &params,
        across,
    );
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        fee_ata,
        params,
        &execute,
    );
    (trigger, execute)
}

fn assert_rejected_unchanged(
    svm: &mut LiteSVM,
    fx: &common::PassiveFixture,
    instructions: &[Instruction],
    signers: &[&Keypair],
    circle_event: Option<Pubkey>,
    context: &str,
) -> TransactionError {
    let before = passive_snapshot(svm, fx, circle_event);
    let error = send_instructions(svm, instructions, &fx.payer, signers)
        .expect_err(context)
        .err
        .clone();
    assert_eq!(passive_snapshot(svm, fx, circle_event), before, "{context}");
    error
}

fn assert_sra_error(error: TransactionError, instruction: u8, expected: SraError) {
    assert_eq!(
        error,
        TransactionError::InstructionError(instruction, InstructionError::Custom(expected.into()),)
    );
}

#[test]
fn passive_trigger_rejects_wrong_missing_and_non_signing_initiator_atomically() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);

    let attacker = funded_keypair(&mut svm, SOL);
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[1].pubkey = attacker.pubkey();
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&attacker, &event],
        Some(event.pubkey()),
        "wrong initiator must reject",
    );
    assert_sra_error(error, 0, SraError::InitiatorNotAllowed);

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts.remove(1);
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&event],
        Some(event.pubkey()),
        "missing initiator must reject",
    );

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[1].is_signer = false;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&event],
        Some(event.pubkey()),
        "non-signing initiator must reject",
    );
    assert_eq!(error, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn passive_trigger_rejects_inactive_sra_and_replaced_initiator_atomically() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    send_ix(
        &mut svm,
        manage_sra_ix(
            fx.owner.pubkey(),
            fx.sra_config,
            fx.initiator.pubkey(),
            false,
        ),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap();
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "inactive SRA must reject",
    );
    assert_sra_error(error, 0, SraError::SraInactive);

    let replacement = funded_keypair(&mut svm, SOL);
    send_ix(
        &mut svm,
        manage_sra_ix(fx.owner.pubkey(), fx.sra_config, replacement.pubkey(), true),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap();
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "replaced initiator must reject",
    );
    assert_sra_error(error, 0, SraError::InitiatorNotAllowed);
}

#[test]
fn passive_trigger_pins_sra_mint_vault_authority_custody_and_adapter() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let other_mint = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 6);
    let substitutes = [
        (2, Pubkey::new_unique()),
        (3, other_mint),
        (4, fx.owner_ata),
        (5, Pubkey::new_unique()),
        (6, fx.owner_ata),
        (7, PROGRAM_ID),
    ];
    for (slot, substitute) in substitutes {
        let (mut trigger, execute, event) =
            cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
        trigger.accounts[slot].pubkey = substitute;
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "substituted trigger account must reject",
        );
    }
}

#[test]
fn passive_trigger_pins_address_book_and_rejects_disabled_entry() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let wrong_entry = setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        10,
        fx.target_token,
        6,
    );
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[8].pubkey = wrong_entry;
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong address-book entry must reject",
    );
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts.remove(8);
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "missing address-book entry must reject",
    );

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
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "disabled address-book entry must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidAddressBook);
}

#[test]
fn passive_trigger_rejects_runtime_route_substitution_atomically() {
    type ParamsMutator = fn(&mut PrepareParams);
    let cases: [ParamsMutator; 3] = [
        |params| params.target_chain_id = 10,
        |params| params.recipient = evm_bytes32(0x99),
        |params| params.target_token = evm_bytes32(0x77),
    ];
    for mutate in cases {
        let mut svm = setup_svm();
        let fx = common::PassiveFixture::new(&mut svm);
        let mut params = sample_prepare_params(fx.target_token);
        mutate(&mut params);
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
        let (trigger, execute, event) =
            cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
        let error = assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "runtime route substitution must reject",
        );
        assert_sra_error(error, 0, SraError::RouteMismatch);
    }
}

#[test]
fn passive_trigger_rejects_unallowlisted_and_non_executable_adapter_atomically() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new_with_adapter_policy(
        &mut svm,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![ADAPTER_PROGRAM_ID],
    );
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unallowlisted adapter must reject",
    );
    assert_sra_error(error, 0, SraError::AdapterNotAllowed);

    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let mut program = svm.get_account(&CCTP_ADAPTER_PROGRAM_ID).unwrap();
    program.executable = false;
    svm.set_account(CCTP_ADAPTER_PROGRAM_ID, program).unwrap();
    let params = sample_prepare_params(fx.target_token);
    let (trigger, _, _) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger],
        &[&fx.initiator],
        None,
        "non-executable adapter must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidAdapterProgram);
}

#[test]
fn passive_trigger_rejects_zero_insufficient_and_exhausted_nonce_atomically() {
    let cases = [(0, 0), (SRA_STARTING_BALANCE + 1, SRA_STARTING_BALANCE)];
    for (input, output) in cases {
        let mut svm = setup_svm();
        let fx = common::PassiveFixture::new(&mut svm);
        let mut params = sample_prepare_params(fx.target_token);
        params.input_amount = input;
        params.output_amount = u256_from_u64(output);
        let cctp = CctpDepositParams::from_prepare(&params, 0, 2_000).unwrap();
        let (trigger, execute, event) = cctp_pair(&fx, params, cctp, None);
        let error = assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "invalid amount must reject",
        );
        let expected = if input == 0 {
            SraError::ZeroInputAmount
        } else {
            SraError::InsufficientSourceBalance
        };
        assert_sra_error(error, 0, expected);
    }

    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    set_sra_config(&mut svm, &fx.sra_config, |state| {
        state.deposit_nonce = u64::MAX
    });
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "exhausted nonce must reject",
    );
    assert_sra_error(error, 0, SraError::ArithmeticOverflow);
}

#[test]
fn passive_trigger_rejects_fee_slippage_and_pairing_mismatches_atomically() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 2_999;
    params.output_amount = u256_from_u64(995_000);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong live fee must reject",
    );
    assert_sra_error(error, 0, SraError::FeeMismatch);

    params.protocol_fee_amount = 3_000;
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "missing fee ATA must reject",
    );
    assert_sra_error(error, 0, SraError::MissingFeeRecipientAccount);

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    trigger.accounts[12].pubkey = fx.owner_ata;
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong fee ATA must reject",
    );

    let mut sponsored = params.clone();
    sponsored.is_sponsor = true;
    sponsored.protocol_fee_amount = 0;
    let (trigger, execute, event) =
        cctp_pair(&fx, sponsored.clone(), cctp_params_for(&sponsored), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unauthorized sponsorship must reject",
    );
    assert_sra_error(error, 0, SraError::SponsoredNotAllowed);

    let mut overridden = params.clone();
    overridden.protocol_fee_bps_override = 50;
    overridden.protocol_fee_amount = 5_000;
    let (trigger, execute, event) = cctp_pair(
        &fx,
        overridden.clone(),
        cctp_params_for(&overridden),
        Some(fee_ata),
    );
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unauthorized override must reject",
    );
    assert_sra_error(error, 0, SraError::FeeMismatch);

    let mut below_floor = params.clone();
    below_floor.output_amount = u256_from_u64(994_999);
    let (trigger, execute, event) = cctp_pair(
        &fx,
        below_floor.clone(),
        cctp_params_for(&below_floor),
        Some(fee_ata),
    );
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "output below gross slippage floor must reject",
    );
    assert_sra_error(error, 0, SraError::SlippageExceeded);

    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger],
        &[&fx.initiator],
        None,
        "missing adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::MissingPairedInstruction);

    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[
            trigger_ix(
                &fx.payer.pubkey(),
                &fx.initiator.pubkey(),
                &fx.accounts,
                &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
                Some(fee_ata),
                params.clone(),
                &execute,
            ),
            compute_budget_ix(),
            execute,
        ],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "non-adjacent adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidPairedInstruction);

    let (trigger, mut execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    *execute.data.last_mut().unwrap() ^= 1;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "data-mismatched adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::PairedInstructionMismatch);
}

#[test]
fn passive_cctp_finality_and_quote_matrix() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    for finality in [0, 1, 999, 1001, 1999, 2001] {
        let cctp = CctpDepositParams::from_prepare(&params, 0, finality).unwrap();
        let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp, None);
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "non-canonical CCTP finality must reject",
        );
    }

    for max_fee in [1_000_000, 1_000_001] {
        let cctp = CctpDepositParams::from_prepare(&params, max_fee, 1_000).unwrap();
        let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp, None);
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "CCTP max fee at or above net must reject",
        );
    }

    let mut excessive_output = params.clone();
    excessive_output.output_amount = u256_from_u64(999_001);
    let cctp = CctpDepositParams::from_prepare(&excessive_output, 1_000, 1_000).unwrap();
    let (trigger, execute, event) = cctp_pair(&fx, excessive_output, cctp, None);
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "CCTP output above net minus max fee must reject",
    );
}

#[test]
fn passive_cctp_rejects_noncanonical_or_non_six_decimal_mint() {
    let mut svm = setup_svm();
    let base = common::PassiveFixture::new(&mut svm);
    let other_mint = create_mint(&mut svm, &base.payer, &base.payer.pubkey(), 6);
    let other_vault = create_ata(&mut svm, &base.payer, &base.sra_config, &other_mint);
    mint_to(
        &mut svm,
        &base.payer,
        &base.payer,
        &other_mint,
        &other_vault,
        SRA_STARTING_BALANCE,
    );
    setup_address_book_entry(
        &mut svm,
        &base.payer,
        &base.owner,
        &base.address_book_root,
        &other_mint,
        base.target_chain_id,
        base.target_token,
        6,
    );
    let other_accounts =
        TriggerAccounts::derive(&base.sra_config, &other_mint, &CCTP_ADAPTER_PROGRAM_ID);
    let across_state = funded_keypair(&mut svm, SOL).pubkey();
    let across_vault = create_ata(&mut svm, &base.payer, &across_state, &other_mint);
    let other = common::PassiveFixture {
        payer: base.payer.insecure_clone(),
        owner: base.owner.insecure_clone(),
        initiator: base.initiator.insecure_clone(),
        fee_admin: base.fee_admin.insecure_clone(),
        address_book_root: base.address_book_root,
        handler_data: base.handler_data.clone(),
        sra_config: base.sra_config,
        source_mint: other_mint,
        target_chain_id: base.target_chain_id,
        target_token: base.target_token,
        adapter_program: base.adapter_program,
        accounts: other_accounts,
        owner_ata: create_ata(&mut svm, &base.payer, &base.owner.pubkey(), &other_mint),
        across_state,
        across_vault,
    };
    let params = sample_prepare_params(other.target_token);
    let (trigger, execute, event) =
        cctp_pair(&other, params.clone(), cctp_params_for(&params), None);
    assert_rejected_unchanged(
        &mut svm,
        &other,
        &[trigger, execute],
        &[&other.initiator, &event],
        Some(event.pubkey()),
        "noncanonical CCTP mint must reject",
    );

    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let mut mint_account = svm.get_account(&fx.source_mint).unwrap();
    let mut mint = litesvm_token::spl_token::state::Mint::unpack(&mint_account.data).unwrap();
    mint.decimals = 5;
    mint.pack_into_slice(&mut mint_account.data);
    svm.set_account(fx.source_mint, mint_account).unwrap();
    let mut params = sample_prepare_params(fx.target_token);
    params.output_amount = u256_from_u64(9_950_000);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "non-six-decimal CCTP mint must reject",
    );
    assert_eq!(
        error,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(CctpAdapterError::InvalidInputMint.into()),
        )
    );
}

#[test]
fn passive_cctp_reused_event_signer_rolls_back() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    send_instructions(
        &mut svm,
        &[trigger, execute],
        &fx.payer,
        &[&fx.initiator, &event],
    )
    .unwrap();

    let (_, mut execute, _) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    execute.accounts[13].pubkey = event.pubkey();
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
        None,
        params,
        &execute,
    );
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "reused Circle event signer must reject",
    );
}

#[test]
fn passive_across_rejects_timing_delegate_and_cpi_failure_atomically() {
    let mut svm = setup_svm();
    let fx = common::PassiveFixture::new_with_adapter_policy(
        &mut svm,
        ADAPTER_PROGRAM_ID,
        50,
        vec![ADAPTER_PROGRAM_ID],
    );
    let params = sample_prepare_params(fx.target_token);
    for (fill_deadline, exclusivity) in [(299, 120), (600, 601)] {
        let across = AcrossDepositParams::from_prepare(
            &params,
            Pubkey::new_unique(),
            fill_deadline,
            exclusivity,
        )
        .unwrap();
        let (trigger, execute) = across_pair(&svm, &fx, params.clone(), across, None);
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator],
            None,
            "invalid Across timing must reject",
        );
    }

    let across =
        AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120).unwrap();
    let (_, mut execute) = across_pair(&svm, &fx, params.clone(), across, None);
    execute.accounts[10].pubkey = Pubkey::new_unique();
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
        None,
        params.clone(),
        &execute,
    );
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator],
        None,
        "wrong Across delegate must reject",
    );
    assert_eq!(
        error,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(AcrossAdapterError::InvalidDelegateAccount.into()),
        )
    );

    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = params;
    params.protocol_fee_amount = 3_000;
    params.output_amount = u256_from_u64(995_000);
    freeze_token_account(&mut svm, &fx.across_vault);
    let across =
        AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120).unwrap();
    let (trigger, execute) = across_pair(&svm, &fx, params, across, Some(fee_ata));
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator],
        None,
        "Across CPI failure must roll back trigger state",
    );
}
