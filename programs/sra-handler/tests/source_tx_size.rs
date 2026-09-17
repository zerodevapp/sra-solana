//! Passive source packet-size regression assertions.

mod common;

use common::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_packet::PACKET_DATA_SIZE;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use sra_common::{constants::MAX_ADAPTERS, source_deposit::AcrossDepositParams};

const PROTOCOL_FEE: u64 = 3_000;

fn compute_budget_ix() -> Instruction {
    solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(
        SOURCE_CU_BUDGET,
    )
}

fn assert_packet_budget(
    label: &str,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
) -> (u64, VersionedTransaction) {
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
    (without_alt_size, with_alt)
}

#[test]
fn passive_initialize_sra_v0_transaction_fits_with_alt() {
    let payer = Keypair::new();
    let owner = Keypair::new();
    let address_book_root = Pubkey::new_unique();
    let source_mint = sra_common::constants::USDC_MINT;
    let mut handler_data = sample_handler_data(address_book_root);
    handler_data.allowed_adapters = (0..MAX_ADAPTERS).map(|_| Pubkey::new_unique()).collect();
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
    let (no_alt_size, _) = assert_packet_budget("initialize SRA", &payer, &[&owner], &[initialize]);
    assert!(no_alt_size <= PACKET_DATA_SIZE as u64);
}

#[test]
fn passive_cctp_trigger_v0_transaction_fits_with_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_params(&fx, PROTOCOL_FEE);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let (no_alt_size, tx) = assert_packet_budget(
        "CCTP trigger",
        &fx.payer,
        &[&fx.initiator, &event],
        &[compute_budget_ix(), trigger, execute],
    );
    assert!(no_alt_size > PACKET_DATA_SIZE as u64);
    assert!(!signer_keys(&tx).contains(&fx.owner.pubkey()));
}

#[test]
fn passive_across_trigger_v0_transaction_fits_with_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let params = fee_params(&fx, PROTOCOL_FEE);
    let across = AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120)
        .expect("valid Across deposit params");
    let (trigger, execute) = across_pair(&fx, params, across, Some(fee_ata));
    let (no_alt_size, tx) = assert_packet_budget(
        "Across trigger",
        &fx.payer,
        &[&fx.initiator],
        &[compute_budget_ix(), trigger, execute],
    );
    assert!(no_alt_size > PACKET_DATA_SIZE as u64);
    assert!(!signer_keys(&tx).contains(&fx.owner.pubkey()));
}
