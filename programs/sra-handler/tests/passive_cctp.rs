mod common;

use common::*;
use solana_sdk::{
    instruction::InstructionError,
    message::{Message, VersionedMessage},
    signer::Signer,
    transaction::{TransactionError, VersionedTransaction},
};
use sra_cctp_adapter::CctpAdapterError;
use sra_common::{
    abi::u256_from_u64, constants::FINALITY_THRESHOLD_FAST, source_deposit::CctpDepositParams,
};

#[test]
fn passive_cctp_happy_path() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_params(&fx, 3_000);
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
    assert!(!signer_keys(&tx).contains(&fx.owner.pubkey()));
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
    assert_eq!(triggered.output_amount, u256_from_u64(FEE_TEST_OUTPUT));
    assert_eq!(triggered.target_chain_id, params.target_chain_id);
    assert_eq!(triggered.recipient, params.recipient);
    assert_eq!(triggered.target_token, params.target_token);
    let executed =
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap();
    assert_eq!(executed.nonce, 0);
}

#[test]
fn passive_cctp_rejects_unsupported_target_chain_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_with_target_chain(&mut svm, 56);
    let mut params = sample_prepare_params(fx.target_token);
    params.target_chain_id = 56;
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unsupported CCTP target chain must reject",
    );
    assert_eq!(
        error,
        TransactionError::InstructionError(
            1,
            InstructionError::Custom(CctpAdapterError::UnsupportedChain.into()),
        )
    );
}

#[test]
fn passive_cctp_finality_and_quote_matrix() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
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
fn passive_cctp_rejects_noncanonical_mint() {
    let mut svm = setup_svm();
    let base = PassiveFixture::new(&mut svm);
    let other_mint = create_mint(&mut svm, &base.payer, &base.payer.pubkey(), 6);
    let across_state = funded_keypair(&mut svm, SOL).pubkey();
    let across_vault = create_ata(&mut svm, &base.payer, &across_state, &other_mint);
    let other = PassiveFixture::for_mint(
        &mut svm,
        &base,
        other_mint,
        CCTP_ADAPTER_PROGRAM_ID,
        across_state,
        across_vault,
    );
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
}

#[test]
fn passive_cctp_reused_event_signer_rolls_back() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
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
