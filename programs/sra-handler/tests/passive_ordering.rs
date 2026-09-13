mod common;

use common::*;
use litesvm::{types::TransactionMetadata, LiteSVM};
use solana_sdk::{
    pubkey::Pubkey,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use sra_common::{abi::u256_from_u64, errors::SraError};

const FEE_BPS: u16 = 30;

fn protocol_fee(amount: u64) -> u64 {
    amount * u64::from(FEE_BPS) / 10_000
}

fn race_fixture() -> (LiteSVM, PassiveFixture, Pubkey) {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, FEE_BPS, false, false);
    svm.expire_blockhash();
    (svm, fx, fee_ata)
}

fn sign_trigger(
    svm: &LiteSVM,
    fx: &PassiveFixture,
    amount: u64,
    fee_ata: Pubkey,
) -> (Transaction, Pubkey) {
    let mut params = sample_prepare_params(fx.target_token);
    params.input_amount = amount;
    params.protocol_fee_amount = protocol_fee(amount);
    params.output_amount = u256_from_u64(amount * 995 / 1_000);
    let (trigger, execute, event) =
        cctp_pair(fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let tx = Transaction::new_signed_with_payer(
        &[trigger, execute],
        Some(&fx.payer.pubkey()),
        &[&fx.payer, &fx.initiator, &event],
        svm.latest_blockhash(),
    );
    assert_eq!(tx.signatures.len(), 3);
    assert!(!tx.message.account_keys.contains(&fx.owner.pubkey()));
    (tx, event.pubkey())
}

fn sign_manage(svm: &LiteSVM, fx: &PassiveFixture, initiator: Pubkey, active: bool) -> Transaction {
    Transaction::new_signed_with_payer(
        &[manage_sra_ix(
            fx.owner.pubkey(),
            fx.sra_config,
            initiator,
            active,
        )],
        Some(&fx.payer.pubkey()),
        &[&fx.payer, &fx.owner],
        svm.latest_blockhash(),
    )
}

fn assert_deposit(
    svm: &LiteSVM,
    fx: &PassiveFixture,
    metadata: &TransactionMetadata,
    circle_event: Pubkey,
    amount: u64,
    nonce: u64,
) {
    let net = amount - protocol_fee(amount);
    let triggered = extract_deposit_triggered(&metadata.logs).unwrap();
    assert_eq!(triggered.nonce, nonce);
    assert_eq!(triggered.input_amount, amount);
    let executed =
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap();
    assert_eq!(executed.nonce, nonce);
    assert_eq!(executed.message_sent_event_data, circle_event);
    assert_eq!(executed.input_amount, net);
    assert_eq!(fetch_sent_deposit_for_burn(svm, &circle_event).amount, net);
    assert_eq!(fetch_token_balance(svm, &fx.accounts.adapter_custody), 0);
    assert_eq!(
        fetch_sra_config(svm, &fx.sra_config).deposit_nonce,
        nonce + 1
    );
}

fn assert_no_deposit(metadata: &TransactionMetadata) {
    assert!(extract_deposit_triggered(&metadata.logs).is_none());
    assert!(
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .is_none()
    );
}

#[test]
fn pause_first_rejects_presigned_trigger_then_recovers_complete_balance() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let (pending, circle_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    let pause = sign_manage(&svm, &fx, fx.initiator.pubkey(), false);
    svm.send_transaction(pause).unwrap();
    let before = passive_snapshot(&svm, &fx, Some(circle_event));
    let error = svm.send_transaction(pending).unwrap_err();
    assert_sra_error(error.err, 0, SraError::SraInactive);
    assert_no_deposit(&error.meta);
    assert_eq!(passive_snapshot(&svm, &fx, Some(circle_event)), before);
    let recovered = recover_source_vault(&mut svm, &fx);
    assert_eq!(recovered.amount, 2_000_000);
    assert_eq!(recovered.deposit_nonce, 0);
    assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), 2_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 0);
    assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), 2_000_000);
    assert!(svm.get_account(&circle_event).is_none());
}

#[test]
fn trigger_first_then_pause_and_recovery_return_only_remainder() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let (trigger, circle_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    let pause = sign_manage(&svm, &fx, fx.initiator.pubkey(), false);
    let metadata = svm.send_transaction(trigger).unwrap();
    assert_deposit(&svm, &fx, &metadata, circle_event, 1_000_000, 0);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000_000);
    let bridge_before = svm.get_account(&circle_event).unwrap();
    svm.send_transaction(pause).unwrap();
    let recovered = recover_source_vault(&mut svm, &fx);
    assert_eq!(recovered.amount, 1_000_000);
    assert_eq!(recovered.deposit_nonce, 1);
    assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 3_000);
    assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), 1_003_000);
    assert_eq!(svm.get_account(&circle_event).unwrap(), bridge_before);
    let config = fetch_sra_config(&svm, &fx.sra_config);
    assert!(!config.active);
    assert_eq!(config.initiator, fx.initiator.pubkey());
}

#[test]
fn initiator_replacement_first_rejects_presigned_old_initiator() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let replacement = Pubkey::new_unique();
    let (pending, circle_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    assert!(!pending.message.account_keys.contains(&replacement));
    let replace = sign_manage(&svm, &fx, replacement, true);
    svm.send_transaction(replace).unwrap();
    let before = passive_snapshot(&svm, &fx, Some(circle_event));
    let error = svm.send_transaction(pending).unwrap_err();
    assert_sra_error(error.err, 0, SraError::InitiatorNotAllowed);
    assert_no_deposit(&error.meta);
    assert_eq!(passive_snapshot(&svm, &fx, Some(circle_event)), before);
    let config = fetch_sra_config(&svm, &fx.sra_config);
    assert_eq!(config.initiator, replacement);
    assert_eq!(config.deposit_nonce, 0);
    assert!(config.active);
}

#[test]
fn trigger_first_then_replacement_preserves_nonce_and_rejects_identical_signed_replay() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let replacement = Pubkey::new_unique();
    let (trigger, circle_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    let replace = sign_manage(&svm, &fx, replacement, true);
    let metadata = svm.send_transaction(trigger.clone()).unwrap();
    assert_deposit(&svm, &fx, &metadata, circle_event, 1_000_000, 0);
    let bridge_before = svm.get_account(&circle_event).unwrap();
    let managed = svm.send_transaction(replace).unwrap();
    let event = extract_anchor_event::<sra_handler::events::SraManaged>(&managed.logs).unwrap();
    assert_eq!(event.deposit_nonce, 1);
    assert_eq!(event.initiator, replacement);
    let config = fetch_sra_config(&svm, &fx.sra_config);
    assert_eq!(config.deposit_nonce, 1);
    assert_eq!(config.initiator, replacement);
    assert!(config.active);
    let before = passive_snapshot(&svm, &fx, Some(circle_event));
    let error = svm.send_transaction(trigger).unwrap_err();
    assert_eq!(error.err, TransactionError::AlreadyProcessed);
    assert_no_deposit(&error.meta);
    assert_eq!(passive_snapshot(&svm, &fx, Some(circle_event)), before);
    assert_eq!(svm.get_account(&circle_event).unwrap(), bridge_before);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 3_000);
    assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), 0);
    assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), 1_003_000);
}

#[test]
fn two_presigned_triggers_consume_distinct_nonces_from_shared_balance() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let (first, first_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    let (second, second_event) = sign_trigger(&svm, &fx, 1_000_000, fee_ata);
    assert_ne!(first.signatures[0], second.signatures[0]);
    let metadata = svm.send_transaction(first).unwrap();
    assert_deposit(&svm, &fx, &metadata, first_event, 1_000_000, 0);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 3_000);
    let first_bridge = svm.get_account(&first_event).unwrap();
    let metadata = svm.send_transaction(second).unwrap();
    assert_deposit(&svm, &fx, &metadata, second_event, 1_000_000, 1);
    assert_eq!(svm.get_account(&first_event).unwrap(), first_bridge);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 6_000);
    assert_eq!(fetch_token_balance(&svm, &fx.owner_ata), 0);
    assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), 6_000);
    let config = fetch_sra_config(&svm, &fx.sra_config);
    assert_eq!(config.initiator, fx.initiator.pubkey());
    assert!(config.active);
}

#[test]
fn competing_full_balance_triggers_charge_and_bridge_only_once() {
    let (mut svm, fx, fee_ata) = race_fixture();
    let (first, first_event) = sign_trigger(&svm, &fx, 2_000_000, fee_ata);
    let (second, second_event) = sign_trigger(&svm, &fx, 2_000_000, fee_ata);
    assert_ne!(first.signatures[0], second.signatures[0]);
    let metadata = svm.send_transaction(first).unwrap();
    assert_deposit(&svm, &fx, &metadata, first_event, 2_000_000, 0);
    assert_eq!(fetch_token_balance(&svm, &fx.accounts.sra_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &fee_ata), 6_000);
    assert_eq!(fetch_mint_supply(&svm, &fx.source_mint), 6_000);
    let first_bridge = svm.get_account(&first_event).unwrap();
    let before = passive_snapshot(&svm, &fx, Some(second_event));
    let error = svm.send_transaction(second).unwrap_err();
    assert_sra_error(error.err, 0, SraError::InsufficientSourceBalance);
    assert_no_deposit(&error.meta);
    assert_eq!(passive_snapshot(&svm, &fx, Some(second_event)), before);
    assert_eq!(svm.get_account(&first_event).unwrap(), first_bridge);
    assert!(svm.get_account(&second_event).is_none());
}
