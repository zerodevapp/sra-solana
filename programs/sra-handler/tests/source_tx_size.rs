//! Paired source-transaction size validation.
//!
//! Solana enforces a hard packet-size limit. The full source deposit
//! flow is `trigger` (handler) + `execute_deposit_cctp`
//! (adapter) packaged as a single versioned transaction, co-signed by the
//! ephemeral CCTP event keypair (a third signature = +64 bytes).

mod common;

use common::*;
use solana_sdk::{
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    packet::PACKET_DATA_SIZE,
    signer::Signer,
    transaction::VersionedTransaction,
};
use sra_common::{abi::u256_from_u64, source_deposit::PrepareParams};

const INPUT: u64 = 1_000_000;
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;

fn build_source_versioned_tx_with_lookup_tables(
    svm: &mut litesvm::LiteSVM,
    fx: &PassiveFixture,
    params: PrepareParams,
    lookup_tables: &[AddressLookupTableAccount],
) -> VersionedTransaction {
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let (execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp_params_for(&params));
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        None,
        params.clone(),
        &execute_ix,
    );

    let blockhash = svm.latest_blockhash();
    let v0_msg = v0::Message::try_compile(
        &fx.payer.pubkey(),
        &[trigger, execute_ix],
        lookup_tables,
        blockhash,
    )
    .expect("compile v0 message");

    VersionedTransaction::try_new(
        VersionedMessage::V0(v0_msg),
        &[&fx.payer, &fx.initiator, &event_data],
    )
    .expect("sign versioned tx")
}

fn build_source_versioned_tx(
    svm: &mut litesvm::LiteSVM,
    fx: &PassiveFixture,
    params: PrepareParams,
) -> VersionedTransaction {
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let (execute_ix, _event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp_params_for(&params));
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        None,
        params.clone(),
        &execute_ix,
    );

    let lookup_table = representative_lookup_table(&[trigger, execute_ix]);
    build_source_versioned_tx_with_lookup_tables(svm, fx, params, &[lookup_table])
}

fn build_source_versioned_tx_without_alt(
    svm: &mut litesvm::LiteSVM,
    fx: &PassiveFixture,
    params: PrepareParams,
) -> VersionedTransaction {
    build_source_versioned_tx_with_lookup_tables(svm, fx, params, &[])
}

#[test]
fn source_2ix_versioned_tx_fits_within_packet_limit() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);

    // The source params are now fixed-size (no variable-length action_idxs), so
    // there is a single payload shape to budget. A regression here points at
    // static-account growth, not routing-payload bloat.
    let params = PrepareParams {
        input_amount: INPUT,
        output_amount: u256_from_u64(OUTPUT_WITHIN_SLIPPAGE),
        ..sample_prepare_params(fx.target_token)
    };

    let tx = build_source_versioned_tx(&mut svm, &fx, params);
    let size = bincode::serialize(&tx).expect("bincode serialize tx").len();

    println!("source 2-IX versioned tx: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "source 2-IX versioned tx is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

#[test]
fn source_2ix_versioned_tx_exceeds_packet_limit_without_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = PrepareParams {
        input_amount: INPUT,
        output_amount: u256_from_u64(OUTPUT_WITHIN_SLIPPAGE),
        ..sample_prepare_params(fx.target_token)
    };
    let tx = build_source_versioned_tx_without_alt(&mut svm, &fx, params);
    let size = bincode::serialize(&tx).expect("bincode serialize tx").len();
    println!("source 2-IX versioned tx without ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(size > PACKET_DATA_SIZE);
}
