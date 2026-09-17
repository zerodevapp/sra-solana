mod common;

use common::*;
use solana_message::{v1::MAX_TRANSACTION_SIZE, VersionedMessage};
use solana_signer::Signer;

#[test]
fn passive_cctp_trigger_v1_transaction_size() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute_ix, event_data) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let transaction = build_v1_transaction(
        &fx.payer,
        &[&fx.initiator, &event_data],
        &[trigger, execute_ix],
        svm.latest_blockhash(),
        DEFAULT_V1_CONFIG,
    )
    .expect("build source v1 transaction");
    let size = serialized_v1_len(&transaction);

    println!(
        "passive CCTP trigger v1: {size} bytes, {} accounts",
        transaction.message.static_account_keys().len()
    );
    assert!(matches!(&transaction.message, VersionedMessage::V1(_)));
    assert_eq!(size, 1_477);
    assert_eq!(transaction.message.static_account_keys().len(), 28);
    assert!(size <= MAX_TRANSACTION_SIZE);
    assert!(!signer_keys(&transaction).contains(&fx.owner.pubkey()));
}
