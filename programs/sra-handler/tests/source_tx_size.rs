//! Source 2-IX versioned-transaction size validation.
//!
//! Solana enforces a hard packet-size limit. The full source deposit
//! flow is `validate_and_prepare` (handler) + `execute_deposit_across`
//! (adapter) packaged as a single versioned transaction.

mod common;

use common::*;
use solana_sdk::{
    message::{v0, VersionedMessage},
    packet::PACKET_DATA_SIZE,
    signer::Signer,
    transaction::VersionedTransaction,
};
use sra_common::{constants::MAX_ACTION_IDXS, source_deposit::PrepareParams};

const INPUT: u64 = 1_000_000;
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;

fn build_source_versioned_tx(
    svm: &mut litesvm::LiteSVM,
    fx: &ValidatePrepareFixture,
    params: PrepareParams,
) -> VersionedTransaction {
    let address_book_entry = fx.address_book_entry(&params.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        params.clone(),
    );
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        fx,
        &fetch_sra_config(svm, &fx.sra_config),
        &params,
        across_params_for(fx, &params),
        current_nonce(svm, &fx.accounts.mint_nonce),
    );

    let blockhash = svm.latest_blockhash();
    let v0_msg = v0::Message::try_compile(
        &fx.payer.pubkey(),
        &[validate_ix, execute_ix],
        &[],
        blockhash,
    )
    .expect("compile v0 message");

    VersionedTransaction::try_new(VersionedMessage::V0(v0_msg), &[&fx.payer, &fx.owner])
        .expect("sign versioned tx")
}

#[test]
fn source_2ix_versioned_tx_max_action_idxs_fits_within_packet_limit() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Worst case: max action_idxs (`MAX_ACTION_IDXS` entries × 8 bytes each,
    // plus the 4-byte vector length prefix, vs ~12 bytes for the typical
    // 2-entry case).
    let params = PrepareParams {
        input_amount: INPUT,
        output_amount: OUTPUT_WITHIN_SLIPPAGE,
        target_token: fx.target_token,
        action_idxs: (0..MAX_ACTION_IDXS as u64).collect(),
    };

    let tx = build_source_versioned_tx(&mut svm, &fx, params);
    let size = bincode::serialize(&tx).expect("bincode serialize tx").len();

    println!("source 2-IX versioned tx (max action_idxs): {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "source 2-IX versioned tx is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

#[test]
fn source_2ix_versioned_tx_empty_action_idxs_fits_within_packet_limit() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Best case: empty action_idxs. Sanity-check that the floor is also under
    // budget; a regression here would point at static-account growth, not at
    // routing-payload bloat.
    let params = PrepareParams {
        input_amount: INPUT,
        output_amount: OUTPUT_WITHIN_SLIPPAGE,
        target_token: fx.target_token,
        action_idxs: vec![],
    };

    let tx = build_source_versioned_tx(&mut svm, &fx, params);
    let size = bincode::serialize(&tx).expect("bincode serialize tx").len();

    println!("source 2-IX versioned tx (empty action_idxs): {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "source 2-IX versioned tx is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}
