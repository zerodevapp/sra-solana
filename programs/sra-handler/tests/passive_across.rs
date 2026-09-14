mod common;

use common::*;
use solana_sdk::{
    instruction::InstructionError,
    message::{Message, VersionedMessage},
    pubkey::Pubkey,
    signer::Signer,
    transaction::{TransactionError, VersionedTransaction},
};
use sra_across_adapter::AcrossAdapterError;
use sra_common::source_deposit::AcrossDepositParams;

#[test]
fn passive_across_happy_path() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    set_source_balance(&mut svm, &fx, 2_000_000);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_params(&fx, 3_000);
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        &fx,
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
    assert!(!signer_keys(&tx).contains(&fx.owner.pubkey()));
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
fn passive_across_rejects_timing_delegate_and_cpi_failure_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    let params = sample_prepare_params(fx.target_token);
    for (fill_deadline, exclusivity) in [(299, 120), (600, 601)] {
        let across = AcrossDepositParams::from_prepare(
            &params,
            Pubkey::new_unique(),
            fill_deadline,
            exclusivity,
        )
        .unwrap();
        let (trigger, execute) = across_pair(&fx, params.clone(), across, None);
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
    let (_, mut execute) = across_pair(&fx, params.clone(), across, None);
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
    let params = fee_params(&fx, 3_000);
    freeze_token_account(&mut svm, &fx.across_vault);
    let across =
        AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120).unwrap();
    let (trigger, execute) = across_pair(&fx, params, across, Some(fee_ata));
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator],
        None,
        "Across CPI failure must roll back trigger state",
    );
}
