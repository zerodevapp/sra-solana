//! Source compute-unit regression assertions.
//!
//! The production source deposit is a single transaction shaped
//! `[compute_budget, trigger (IX 1), execute_deposit_cctp
//! (IX 2)]` over an ALT, requesting a 400k CU limit. These tests execute the
//! full 2-IX flow through the mock CCTP program, read the real consumption from
//! LiteSVM's `TransactionMetadata`, log it with headroom, and assert it stays
//! under that production budget — so an Anchor/dependency upgrade (or our own
//! code) that silently doubles the CU cost fails here instead of on mainnet.
//!
//! Measurement notes:
//! - The reported CU includes the compute-budget instruction's own fixed cost,
//!   which production pays identically — so the comparison is apples-to-apples
//!   and, if anything, conservative.
//! - The flow is built as a legacy tx (no ALT) where production uses a v0 tx
//!   over an ALT. ALTs compress account addresses in the serialized message
//!   (tx *size*, pinned by `source_tx_size.rs`); execution CU is unaffected, so
//!   this measures the production CU faithfully.

mod common;

use common::*;
use litesvm::LiteSVM;
use solana_sdk::{compute_budget::ComputeBudgetInstruction, pubkey::Pubkey, signer::Signer};
use sra_common::{abi::u256_from_u64, source_deposit::PrepareParams};

// `SOURCE_CU_BUDGET` (the production-requested 400k limit) is the shared
// constant from `common` — the same value `compute_budget_ix` requests.

/// Runtime CU limit for measurement: the Solana maximum
const MEASUREMENT_CU_LIMIT: u32 = 1_400_000;

const DEFAULT_FEE_BPS: u16 = 30;
const DEFAULT_FEE: u64 = 3_000; // sample input (1_000_000) * 30 / 10_000

/// Builds the production-shaped `[compute_budget, IX 1, IX 2]` source flow,
/// executes it, and returns the CU consumed.
fn measure_source_2ix_cu(
    svm: &mut LiteSVM,
    fx: &PassiveFixture,
    params: PrepareParams,
    fee_recipient_token_account: Option<Pubkey>,
) -> u64 {
    let (execute_ix, event_data) =
        execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp_params_for(&params));
    let address_book_entry = fx.address_book_entry(params.target_chain_id, &params.target_token);
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        fee_recipient_token_account,
        params.clone(),
        &execute_ix,
    );
    let cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(MEASUREMENT_CU_LIMIT);

    send_instructions(
        svm,
        &[cu_limit, trigger, execute_ix],
        &fx.payer,
        &[&fx.initiator, &event_data],
    )
    .expect("source 2-IX flow must execute")
    .compute_units_consumed
}

fn assert_under_budget(consumed: u64, label: &str) {
    // `SOURCE_CU_BUDGET` is a u32 (the runtime CU-limit type); widen losslessly
    // to compare against the u64 `compute_units_consumed`.
    let budget = u64::from(SOURCE_CU_BUDGET);
    let headroom = budget.saturating_sub(consumed);
    println!("source 2-IX flow CU [{label}]: {consumed} / {budget} (headroom {headroom})");
    assert!(
        consumed < budget,
        "source 2-IX flow consumed {consumed} CU [{label}], exceeds the {budget} CU \
         production budget",
    );
}

#[test]
fn source_2ix_flow_under_cu_budget() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let consumed =
        measure_source_2ix_cu(&mut svm, &fx, sample_prepare_params(fx.target_token), None);
    assert_under_budget(consumed, "representative, no fee");
}

#[test]
fn source_2ix_flow_with_fee_under_cu_budget() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false, false);

    // Heaviest production path: protocol fee ON adds a second token CPI
    // (vault -> fee-recipient `transfer_checked`). The quote drops to the
    // gross slippage floor so it stays within the fee-reduced net.
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = DEFAULT_FEE;
    params.output_amount = u256_from_u64(995_000);

    let consumed = measure_source_2ix_cu(&mut svm, &fx, params, Some(fee_ata));
    assert_under_budget(consumed, "fee on");
}
