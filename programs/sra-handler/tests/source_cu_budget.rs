//! Source compute-unit regression assertions.
//!
//! The production source deposit is a single transaction shaped
//! `[compute_budget, validate_and_prepare (IX 1), execute_deposit_across
//! (IX 2)]` over an ALT, requesting a 400k CU limit. These tests execute the
//! full 2-IX flow through the mock SpokePool, read the real consumption from
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
use sra_common::{constants::MAX_ACTION_IDXS, source_deposit::PrepareParams};

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
    fx: &ValidatePrepareFixture,
    params: PrepareParams,
    fee_recipient_token_account: Option<Pubkey>,
) -> u64 {
    let execute_ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        fx,
        &fetch_sra_config(svm, &fx.sra_config),
        &params,
        across_params_for(fx, &params),
        current_nonce(svm, &fx.accounts.mint_nonce),
    );
    let address_book_entry = fx.address_book_entry(params.target_chain_id, &params.target_token);
    let validate_ix = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &fx.address_book_root,
        &address_book_entry,
        fee_recipient_token_account,
        fx.handler_data.clone(),
        params.clone(),
        &execute_ix,
    );
    let cu_limit = ComputeBudgetInstruction::set_compute_unit_limit(MEASUREMENT_CU_LIMIT);

    send_instructions(
        svm,
        &[cu_limit, validate_ix, execute_ix],
        &fx.payer,
        &[&fx.owner],
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

/// Enables a global recipient + a default protocol fee for the fixture's
/// committed app_id, and returns the canonical recipient fee ATA.
fn enable_protocol_fee(svm: &mut LiteSVM, fx: &ValidatePrepareFixture) -> Pubkey {
    let recipient = Pubkey::new_unique();
    send_instructions(
        svm,
        &[
            set_fee_recipient_ix(&fx.fee_admin.pubkey(), recipient),
            set_protocol_fee_ix(
                &fx.payer.pubkey(),
                &fx.fee_admin.pubkey(),
                fx.handler_data.app_id,
                DEFAULT_FEE_BPS,
                false,
            ),
        ],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("fee state setup must succeed");
    create_ata(svm, &fx.payer, &recipient, &fx.source_mint)
}

fn max_action_idxs_params(target_token: [u8; 32]) -> PrepareParams {
    PrepareParams {
        action_idxs: (0..MAX_ACTION_IDXS as u64).collect(),
        ..sample_prepare_params(target_token)
    }
}

#[test]
fn source_2ix_flow_under_cu_budget() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let consumed =
        measure_source_2ix_cu(&mut svm, &fx, sample_prepare_params(fx.target_token), None);
    assert_under_budget(consumed, "representative, no fee");
}

#[test]
fn source_2ix_flow_max_action_idxs_under_cu_budget() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // Largest routing payload: MAX_ACTION_IDXS entries
    let consumed =
        measure_source_2ix_cu(&mut svm, &fx, max_action_idxs_params(fx.target_token), None);
    assert_under_budget(consumed, "max action_idxs, no fee");
}

#[test]
fn source_2ix_flow_with_fee_max_action_idxs_under_cu_budget() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx);

    // Heaviest production path: protocol fee ON adds a second token CPI
    // (vault -> fee-recipient `transfer_checked`) on top of the max-action_idxs
    // payload.
    let mut params = max_action_idxs_params(fx.target_token);
    params.protocol_fee_amount = DEFAULT_FEE;

    let consumed = measure_source_2ix_cu(&mut svm, &fx, params, Some(fee_ata));
    assert_under_budget(consumed, "fee on + max action_idxs");
}
