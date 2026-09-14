//! Passive source packet-size regression assertions.

mod common;

use common::*;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    packet::PACKET_DATA_SIZE,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};
use sra_common::{
    abi::u256_from_u64,
    constants::MAX_ADAPTERS,
    source_deposit::{AcrossDepositParams, PrepareParams},
};

const GROSS_AMOUNT: u64 = 1_000_000;
const PROTOCOL_FEE: u64 = 3_000;
const OUTPUT_AMOUNT: u64 = 995_000;

fn compile_v0(
    payer: &Keypair,
    additional_signers: &[&Keypair],
    ixs: &[Instruction],
    lookup_tables: &[AddressLookupTableAccount],
) -> VersionedTransaction {
    let message = v0::Message::try_compile(&payer.pubkey(), ixs, lookup_tables, Hash::default())
        .expect("compile v0 message");
    let mut signers = vec![payer];
    signers.extend_from_slice(additional_signers);
    VersionedTransaction::try_new(VersionedMessage::V0(message), &signers)
        .expect("sign versioned tx")
}

fn fee_prepare_params(target_token: [u8; 32]) -> PrepareParams {
    let mut params = sample_prepare_params(target_token);
    params.input_amount = GROSS_AMOUNT;
    params.protocol_fee_amount = PROTOCOL_FEE;
    params.output_amount = u256_from_u64(OUTPUT_AMOUNT);
    params
}

fn assert_packet_budget(
    label: &str,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
) -> VersionedTransaction {
    let lookup_table = representative_lookup_table(ixs);
    let with_alt = compile_v0(payer, signers, ixs, &[lookup_table]);
    let without_alt = compile_v0(payer, signers, ixs, &[]);
    let with_alt_size = bincode::serialized_size(&with_alt).expect("serialize ALT transaction");
    let without_alt_size = bincode::serialized_size(&without_alt).expect("serialize transaction");
    println!("passive {label} v0 bytes: ALT={with_alt_size}, no ALT={without_alt_size}, limit={PACKET_DATA_SIZE}");
    assert!(with_alt
        .message
        .address_table_lookups()
        .is_some_and(|tables| !tables.is_empty()));
    assert!(
        with_alt_size <= PACKET_DATA_SIZE as u64,
        "{label}: {with_alt_size} exceeds packet limit"
    );
    with_alt
}

fn assert_owner_not_signer(tx: &VersionedTransaction, owner: Pubkey) {
    let signer_keys = &tx.message.static_account_keys()
        [..usize::from(tx.message.header().num_required_signatures)];
    assert!(
        !signer_keys.contains(&owner),
        "owner must not sign a passive trigger"
    );
}

#[test]
fn passive_initialize_sra_v0_transaction_fits_with_alt() {
    let payer = Keypair::new();
    let owner = Keypair::new();
    let address_book_root = Pubkey::new_unique();
    let source_mint = sra_common::constants::USDC_MINT;
    let mut handler_data = sample_handler_data(address_book_root);
    handler_data.allowed_adapters = (0..MAX_ADAPTERS)
        .map(|index| match index {
            0 => CCTP_ADAPTER_PROGRAM_ID,
            1 => ADAPTER_PROGRAM_ID,
            _ => Pubkey::new_unique(),
        })
        .collect();
    let salt = [0xAB; 32];
    let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let initialize = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root,
        &sra_config,
        handler_data,
        salt,
        Pubkey::new_unique(),
        source_mint,
    );
    assert_packet_budget("initialize SRA", &payer, &[&owner], &[initialize]);
}

#[test]
fn passive_cctp_trigger_v0_transaction_fits_with_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_prepare_params(fx.target_token);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let tx = assert_packet_budget(
        "CCTP trigger",
        &fx.payer,
        &[&fx.initiator, &event],
        &[compute_budget_ix(), trigger, execute],
    );
    assert_owner_not_signer(&tx, fx.owner.pubkey());
}

#[test]
fn passive_across_trigger_v0_transaction_fits_with_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_prepare_params(fx.target_token);
    let across = AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120)
        .expect("valid Across deposit params");
    let (trigger, execute) = across_pair(&fx, params, across, Some(fee_ata));
    let tx = assert_packet_budget(
        "Across trigger",
        &fx.payer,
        &[&fx.initiator],
        &[compute_budget_ix(), trigger, execute],
    );
    assert_owner_not_signer(&tx, fx.owner.pubkey());
}
