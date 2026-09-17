mod common;

use common::v1::*;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{v1::MessageError, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;

#[test]
fn builds_a_signed_v1_transaction_with_explicit_limits() {
    let payer = Keypair::new();
    let signer = Keypair::new();
    let instruction = Instruction {
        program_id: Pubkey::new_unique(),
        accounts: vec![AccountMeta::new_readonly(signer.pubkey(), true)],
        data: vec![1, 2, 3],
    };
    let transaction = build_v1_transaction(
        &payer,
        &[&signer],
        &[instruction],
        Hash::default(),
        DEFAULT_V1_CONFIG,
    )
    .expect("build v1 transaction");

    let VersionedMessage::V1(message) = &transaction.message else {
        panic!("expected v1");
    };
    assert_eq!(message.serialize()[0], 0x81);
    assert_eq!(message.config.compute_unit_limit, Some(400_000));
    assert_eq!(
        message.config.loaded_accounts_data_size_limit,
        Some(67_108_864)
    );
    assert_eq!(transaction.signatures.len(), 2);
}

#[test]
fn rejects_an_oversized_address_set() {
    let payer = Keypair::new();
    let instruction = Instruction {
        program_id: Pubkey::new_unique(),
        accounts: (0..63)
            .map(|_| AccountMeta::new_readonly(Pubkey::new_unique(), false))
            .collect(),
        data: vec![],
    };
    let message = compile_v1_message(
        &payer.pubkey(),
        &[instruction],
        Hash::default(),
        DEFAULT_V1_CONFIG,
    )
    .expect("compile message");

    assert_eq!(message.validate(), Err(MessageError::TooManyAddresses));
}

#[test]
fn rejects_missing_mandatory_resource_limits() {
    let payer = Keypair::new();
    let error = build_v1_transaction(
        &payer,
        &[],
        &[],
        Hash::default(),
        solana_message::v1::TransactionConfig::empty(),
    )
    .expect_err("empty v1 config must fail");

    assert!(error.to_string().contains("resource limits"));
}

#[test]
fn accepts_the_payer_in_the_additional_signer_set() {
    let payer = Keypair::new();
    let transaction =
        build_v1_transaction(&payer, &[&payer], &[], Hash::default(), DEFAULT_V1_CONFIG)
            .expect("deduplicate payer");

    assert_eq!(transaction.signatures.len(), 1);
}
