//! Source protocol fee collection through `validate_and_prepare`.
//!
//! The fee is resolved from the COMMITTED `handler_data.app_id` plus live
//! admin fee state, equality-checked against the attested
//! `params.protocol_fee_amount` (`FeeMismatch`), collected from the SRA vault
//! into the canonical recipient ATA, and the NET (input - fee) is forwarded
//! into adapter custody and bridged. Slippage keeps flooring on the GROSS
//! input (EVM parity).

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signer::Signer, transaction::TransactionError,
};
use sra_common::abi::u256_from_u64;
use sra_common::constants::NO_FEE_BPS_OVERRIDE;
use sra_common::errors::SraError;
use sra_common::source_deposit::PrepareParams;

const INPUT: u64 = 1_000_000;
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const DEFAULT_FEE_BPS: u16 = 30;
const DEFAULT_FEE: u64 = 3_000; // 1_000_000 * 30 / 10_000

/// Anchor framework error codes pinned by the substitution suites.
const ANCHOR_CONSTRAINT_SEEDS: u32 = ErrorCode::ConstraintSeeds as u32;
const ANCHOR_CONSTRAINT_TOKEN_OWNER: u32 = ErrorCode::ConstraintTokenOwner as u32;
const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = ErrorCode::AccountNotInitialized as u32;

fn fee_params(fx: &ValidatePrepareFixture, protocol_fee_amount: u64) -> PrepareParams {
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = protocol_fee_amount;
    params
}

/// Configures live fee state: recipient set + per-app protocol fee for the
/// fixture's committed app_id. Returns the canonical recipient fee ATA.
fn enable_protocol_fee(
    svm: &mut LiteSVM,
    fx: &ValidatePrepareFixture,
    default_fee_bps: u16,
    allow_override: bool,
) -> Pubkey {
    let recipient = Pubkey::new_unique();
    send_instructions(
        svm,
        &[
            set_fee_recipient_ix(&fx.fee_admin.pubkey(), recipient),
            set_protocol_fee_ix(
                &fx.payer.pubkey(),
                &fx.fee_admin.pubkey(),
                fx.handler_data.app_id,
                default_fee_bps,
                allow_override,
            ),
        ],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("fee state setup must succeed");
    create_ata(svm, &fx.payer, &recipient, &fx.source_mint)
}

fn expect_custom_error(
    result: Result<
        litesvm::types::TransactionMetadata,
        Box<litesvm::types::FailedTransactionMetadata>,
    >,
    code: u32,
    context: &str,
) {
    let err = result.expect_err(context);
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::Custom(code)),
        "{context}",
    );
}

/// Rejections must not consume the nonce or move any balance (the whole tx
/// reverts atomically; this pins the fail-closed contract).
fn assert_deposit_state_untouched(svm: &LiteSVM, fx: &ValidatePrepareFixture, fee_ata: &Pubkey) {
    assert_eq!(
        fetch_token_balance(svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "owner ATA untouched on rejection",
    );
    assert_eq!(
        fetch_token_balance(svm, fee_ata),
        0,
        "fee ATA untouched on rejection",
    );
    assert_eq!(
        fetch_token_balance(svm, &fx.across_vault),
        0,
        "across vault untouched on rejection",
    );
    assert_eq!(
        current_nonce(svm, &fx.accounts.mint_nonce),
        0,
        "nonce not consumed on rejection",
    );
}

#[test]
fn fee_collected_happy_path() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    let params = fee_params(&fx, DEFAULT_FEE);
    let meta = fx
        .invoke_with_fee_ata(&mut svm, params.clone(), Some(fee_ata))
        .expect("fee-bearing deposit must succeed");

    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - INPUT,
        "owner pays the gross input",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fee_ata),
        DEFAULT_FEE,
        "fee recipient ATA receives the protocol fee",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.sra_vault),
        0,
        "SRA vault is transient",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        0,
        "adapter custody drained by IX 2",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.across_vault),
        INPUT - DEFAULT_FEE,
        "Across vault receives the NET of the protocol fee",
    );
    assert_eq!(current_nonce(&svm, &fx.accounts.mint_nonce), 1);

    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event must be emitted");
    assert_eq!(ev.input_amount, INPUT, "event carries the gross input");
    assert_eq!(
        ev.protocol_fee_amount, DEFAULT_FEE,
        "event carries the collected protocol fee",
    );
    assert_eq!(ev.output_amount, u256_from_u64(OUTPUT_WITHIN_SLIPPAGE));
}

#[test]
fn fee_mismatch_rejected_preserves_state() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    let result = fx.invoke_with_fee_ata(&mut svm, fee_params(&fx, DEFAULT_FEE - 1), Some(fee_ata));

    let err = result.expect_err("understated fee must reject");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::FeeMismatch.into())
        ),
    );
    assert_deposit_state_untouched(&svm, &fx, &fee_ata);
}

#[test]
fn stale_fee_after_admin_update_fails_closed() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    // Client computes against 30 bps...
    let params = fee_params(&fx, DEFAULT_FEE);

    // ...the admin re-prices the app before the deposit lands.
    send_instructions(
        &mut svm,
        &[set_protocol_fee_ix(
            &fx.payer.pubkey(),
            &fx.fee_admin.pubkey(),
            fx.handler_data.app_id,
            50,
            false,
        )],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("admin re-price must succeed");

    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata)),
        SraError::FeeMismatch.into(),
        "racing admin fee update must fail closed",
    );
}

#[test]
fn sponsor_skips_fee() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    let mut params = fee_params(&fx, 0);
    params.is_sponsor = true;

    fx.invoke_with_fee_ata(&mut svm, params, None)
        .expect("sponsored deposit skips the fee without a fee ATA");

    assert_eq!(
        fetch_token_balance(&svm, &fx.across_vault),
        INPUT,
        "sponsored deposit bridges the full gross input",
    );
}

#[test]
fn override_honored_when_allowed() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, true);

    let mut params = fee_params(&fx, 5_000); // 1_000_000 * 50 / 10_000
    params.protocol_fee_bps_override = 50;

    fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata))
        .expect("honored override must succeed");

    assert_eq!(fetch_token_balance(&svm, &fee_ata), 5_000);
    assert_eq!(fetch_token_balance(&svm, &fx.across_vault), INPUT - 5_000);
}

#[test]
fn override_without_flag_silently_ignored() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    // EVM parity: with allowOverride=false the override is ignored, not
    // refused — the deposit succeeds charging the DEFAULT rate.
    let mut params = fee_params(&fx, DEFAULT_FEE);
    params.protocol_fee_bps_override = 50;

    fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata))
        .expect("disallowed override is silently ignored");

    assert_eq!(
        fetch_token_balance(&svm, &fee_ata),
        DEFAULT_FEE,
        "default rate applies, not the override",
    );
}

#[test]
fn override_above_max_rejected_when_allowed() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, true);

    let mut params = fee_params(&fx, 0);
    params.protocol_fee_bps_override = 1_001; // MAX_FEE_BPS + 1

    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata)),
        SraError::FeeTooHigh.into(),
        "out-of-bounds honored override must reject",
    );
}

#[test]
fn asset_tier_applied_over_default() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);
    send_instructions(
        &mut svm,
        &[set_asset_fee_tier_ix(
            &fx.payer.pubkey(),
            &fx.fee_admin.pubkey(),
            fx.handler_data.app_id,
            fx.source_mint,
            500_000, // INPUT >= threshold → above_or_equal_bps
            10,
            20,
        )],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("tier setup must succeed");

    let params = fee_params(&fx, 2_000); // 1_000_000 * 20 / 10_000
    fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata))
        .expect("tiered deposit must succeed");

    assert_eq!(
        fetch_token_balance(&svm, &fee_ata),
        2_000,
        "tier rate wins over the app default",
    );
}

#[test]
fn removed_protocol_fee_charges_zero() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);
    send_instructions(
        &mut svm,
        &[remove_protocol_fee_ix(
            &fx.fee_admin.pubkey(),
            fx.handler_data.app_id,
        )],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("fee removal must succeed");

    // Closed PDA == EVM `isSet = false`: existence is read on-chain, so the
    // deposit charges zero without any caller-side claim.
    fx.invoke_with_fee_ata(&mut svm, fee_params(&fx, 0), Some(fee_ata))
        .expect("deposit after fee removal charges zero");

    assert_eq!(fetch_token_balance(&svm, &fee_ata), 0);
    assert_eq!(fetch_token_balance(&svm, &fx.across_vault), INPUT);
}

#[test]
fn recipient_unset_charges_zero() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    // Protocol fee configured, but the global recipient stays
    // Pubkey::default() (the fixture's initial state) — fees globally off.
    send_instructions(
        &mut svm,
        &[set_protocol_fee_ix(
            &fx.payer.pubkey(),
            &fx.fee_admin.pubkey(),
            fx.handler_data.app_id,
            DEFAULT_FEE_BPS,
            false,
        )],
        &fx.payer,
        &[&fx.fee_admin],
    )
    .expect("fee config without recipient must succeed");

    fx.invoke(&mut svm, fee_params(&fx, 0))
        .expect("recipient-unset deposit charges zero");

    assert_eq!(fetch_token_balance(&svm, &fx.across_vault), INPUT);
}

#[test]
fn fee_ata_missing_when_fee_positive_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, fee_params(&fx, DEFAULT_FEE), None),
        SraError::MissingFeeRecipientAccount.into(),
        "positive fee without a recipient ATA must reject",
    );
}

#[test]
fn fee_ata_substitution_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    // Wrong authority: the owner's own ATA (exists, right mint) — the
    // associated_token::authority binding to the COMMITTED fee recipient
    // fires as ConstraintTokenOwner.
    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, fee_params(&fx, DEFAULT_FEE), Some(fx.owner_ata)),
        ANCHOR_CONSTRAINT_TOKEN_OWNER,
        "fee ATA with the wrong authority must reject",
    );

    // Non-canonical address: an arbitrary (nonexistent) key in the fee-ATA
    // slot fails typed deserialization before any transfer.
    expect_custom_error(
        fx.invoke_with_fee_ata(
            &mut svm,
            fee_params(&fx, DEFAULT_FEE),
            Some(Pubkey::new_unique()),
        ),
        ANCHOR_ACCOUNT_NOT_INITIALIZED,
        "non-canonical fee ATA must reject",
    );
}

#[test]
fn fee_config_pda_substitution_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    // The three fee accounts are bound to committed state — substitution
    // (and therefore claimed absence) is structurally impossible. This pins
    // the closure of the Option-account fee bypass: the per-app PDAs are
    // seeds-constrained UncheckedAccounts (ConstraintSeeds), and the typed
    // fee_config singleton fails deserialization at a substituted address
    // before any constraint or body logic runs.
    let cases = [
        (
            derive_fee_config_pda().0,
            ANCHOR_ACCOUNT_NOT_INITIALIZED,
            "substituted fee_config must reject",
        ),
        (
            derive_protocol_fee_pda(&fx.handler_data.app_id).0,
            ANCHOR_CONSTRAINT_SEEDS,
            "substituted protocol_fee_config must fail ConstraintSeeds",
        ),
        (
            derive_asset_fee_tier_pda(&fx.handler_data.app_id, &fx.source_mint).0,
            ANCHOR_CONSTRAINT_SEEDS,
            "substituted asset_fee_tier must fail ConstraintSeeds",
        ),
    ];
    for (victim, expected_code, context) in cases {
        let result = fx.invoke_with_fee_accounts_mutation(
            &mut svm,
            fee_params(&fx, DEFAULT_FEE),
            Some(fee_ata),
            victim,
            Pubkey::new_unique(),
        );
        expect_custom_error(result, expected_code, context);
    }
}

#[test]
fn slippage_floors_on_gross_with_fee() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, DEFAULT_FEE_BPS, false);

    // Gross floor at 50 bps slippage = 995_000. Net (997_000) would floor at
    // 992_015 — an output of 994_000 discriminates: it clears the net-derived
    // floor but NOT the gross floor. EVM applies slippage to the
    // gross deposit, so this must reject.
    let mut params = fee_params(&fx, DEFAULT_FEE);
    params.output_amount = u256_from_u64(994_000);

    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, params, Some(fee_ata)),
        SraError::SlippageExceeded.into(),
        "slippage must floor on the gross input, not the net",
    );
}

#[test]
fn zero_fee_params_required_when_fee_disabled() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    // No fee state at all (fixture default): a deposit claiming a positive
    // fee must FeeMismatch (the recomputed expectation is 0). No fee ATA is
    // supplied — the equality check rejects before the ATA is ever needed.
    expect_custom_error(
        fx.invoke_with_fee_ata(&mut svm, fee_params(&fx, 1), None),
        SraError::FeeMismatch.into(),
        "claimed fee with no fee state must reject",
    );
}

#[test]
fn legacy_zero_fee_params_unchanged() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = fee_params(&fx, 0);
    assert_eq!(params.protocol_fee_amount, 0);
    assert!(!params.is_sponsor);
    assert_eq!(params.protocol_fee_bps_override, NO_FEE_BPS_OVERRIDE);

    fx.invoke(&mut svm, params)
        .expect("zero-fee deposit flows unchanged");
    assert_eq!(fetch_token_balance(&svm, &fx.across_vault), INPUT);
}
