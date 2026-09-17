use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{v1, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::error::Error;

pub type V1Result<T> = Result<T, Box<dyn Error>>;

pub const DEFAULT_V1_CONFIG: v1::TransactionConfig = v1::TransactionConfig::empty()
    .with_compute_unit_limit(400_000)
    .with_loaded_accounts_data_size_limit(67_108_864);

pub const fn v1_config_with_compute_limit(limit: u32) -> v1::TransactionConfig {
    v1::TransactionConfig::empty()
        .with_compute_unit_limit(limit)
        .with_loaded_accounts_data_size_limit(67_108_864)
}

pub fn compile_v1_message(
    payer: &solana_pubkey::Pubkey,
    instructions: &[Instruction],
    blockhash: Hash,
    config: v1::TransactionConfig,
) -> V1Result<v1::Message> {
    Ok(v1::Message::try_compile_with_config(
        payer,
        instructions,
        blockhash,
        config,
    )?)
}

pub fn serialized_v1_len(transaction: &VersionedTransaction) -> usize {
    wincode::serialize(transaction)
        .expect("serialize v1 transaction")
        .len()
}

pub fn build_v1_transaction(
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
    blockhash: Hash,
    config: v1::TransactionConfig,
) -> V1Result<VersionedTransaction> {
    let message = compile_v1_message(&payer.pubkey(), instructions, blockhash, config)?;
    if message.config.compute_unit_limit.is_none()
        || message.config.loaded_accounts_data_size_limit.is_none()
    {
        return Err("v1 resource limits must be explicit".into());
    }
    message.validate()?;
    let mut all_signers = vec![payer];
    for signer in signers {
        if all_signers
            .iter()
            .all(|existing| existing.pubkey() != signer.pubkey())
        {
            all_signers.push(signer);
        }
    }
    let transaction = VersionedTransaction::try_new(VersionedMessage::V1(message), &all_signers)?;
    let size = serialized_v1_len(&transaction);
    if size > v1::MAX_TRANSACTION_SIZE {
        return Err(format!("v1 transaction is {size} bytes; maximum is 4096").into());
    }
    Ok(transaction)
}
