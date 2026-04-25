mod common;

use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use sra_common::{
    abi::{
        abi_encode_sra_message, build_extra_envelope, bytes32_to_evm_address, source_token_hash,
        u256_from_u64, DepositInfo, Message, TokenParam,
    },
    constants::{MAX_ACTION_IDXS, SOLANA_CHAIN_ID},
    errors::SraError,
};
use sra_handler::instructions::source::validate_and_prepare::PrepareParams;

const INPUT: u64 = 1_000_000;
// 50 bps slippage tolerance -> min acceptable = 995_000 for INPUT = 1_000_000.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const DEFAULT_ACTION_IDXS: [u64; 2] = [3, 7];

/// Byte-for-byte reproduction of what `validate_and_prepare` should emit as
/// `DepositPrepared.message` for a given pre-increment nonce. Mirrors the
/// instruction's Step 4 exactly; any drift here or in the on-chain encoder
/// breaks this test immediately.
fn expected_canonical_message(
    fx: &ValidatePrepareFixture,
    nonce: u64,
    params: &PrepareParams,
) -> Vec<u8> {
    let target_adapter_evm = bytes32_to_evm_address(&fx.config.target_adapter).unwrap();
    let target_handler_evm = bytes32_to_evm_address(&fx.config.target_handler).unwrap();
    let target_token_evm = bytes32_to_evm_address(&params.target_token).unwrap();

    let extra = build_extra_envelope(&target_adapter_evm, nonce);
    let action_idxs: Vec<[u8; 32]> = params
        .action_idxs
        .iter()
        .map(|&i| u256_from_u64(i))
        .collect();

    let msg = Message {
        account: fx.config.recipient,
        init_code: vec![],
        deposit_info: DepositInfo {
            deposit_param: TokenParam {
                token: source_token_hash(&fx.source_mint),
                amount: u256_from_u64(params.input_amount),
            },
            target_chain_id: u256_from_u64(fx.config.target_chain_id),
            target_param: TokenParam {
                token: target_token_evm,
                amount: u256_from_u64(params.output_amount),
            },
            extra,
        },
        action_idxs,
        app_id: fx.config.app_id,
    };

    abi_encode_sra_message(
        &target_adapter_evm,
        &target_handler_evm,
        &u256_from_u64(SOLANA_CHAIN_ID),
        &msg,
    )
}

fn default_prepare_params(fx: &ValidatePrepareFixture) -> PrepareParams {
    PrepareParams {
        input_amount: INPUT,
        output_amount: OUTPUT_WITHIN_SLIPPAGE,
        target_token: fx.target_token,
        action_idxs: DEFAULT_ACTION_IDXS.to_vec(),
    }
}

#[test]
fn validate_and_prepare_happy_path() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let meta = fx
        .invoke(&mut svm, params.clone())
        .expect("validate_and_prepare should succeed");

    // Tokens flowed owner -> SRA vault -> adapter custody in a single tx.
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - INPUT,
        "owner ATA debited",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.sra_vault),
        0,
        "SRA vault is transient — tokens pass through in the same tx",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        INPUT,
        "adapter custody holds the locked tokens",
    );

    // Nonce bumped 0 -> 1.
    let nonce_state = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
    assert_eq!(nonce_state.nonce, 1);

    // DepositPrepared event emitted with the canonical message bytes.
    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event must be emitted");
    assert_eq!(ev.sra_config, fx.sra_config);
    assert_eq!(ev.source_mint, fx.source_mint);
    assert_eq!(ev.adapter_program, fx.adapter_program);
    assert_eq!(ev.nonce, 0, "event carries the pre-increment nonce");
    assert_eq!(ev.input_amount, INPUT);
    assert_eq!(ev.output_amount, OUTPUT_WITHIN_SLIPPAGE);

    let expected = expected_canonical_message(&fx, 0, &params);
    assert_eq!(
        ev.message, expected,
        "emitted message must match abi_encode_sra_message byte-for-byte",
    );
}

#[test]
fn validate_and_prepare_disabled_address_book_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.sra_config,
        fx.target_chain_id,
        fx.target_token,
        0,
    );

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("disabled address book must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAddressBook.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_unregistered_adapter_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![],
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("clear adapters");

    let err = fx
        .invoke(&mut svm, default_prepare_params(&fx))
        .expect_err("unregistered adapter must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::AdapterNotAllowed.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_slippage_exceeded_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                output_amount: 900_000,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("slippage must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::SlippageExceeded.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_too_many_action_idxs_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let too_many = (0..=MAX_ACTION_IDXS as u64).collect::<Vec<_>>();
    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                action_idxs: too_many,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("too many action idxs must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::TooManyActionIdxs.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_insufficient_balance_rejected() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let over_balance = OWNER_STARTING_BALANCE + 1;
    let err = fx
        .invoke(
            &mut svm,
            PrepareParams {
                input_amount: over_balance,
                output_amount: over_balance,
                ..default_prepare_params(&fx)
            },
        )
        .expect_err("over-balance transfer must fail")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::Custom(1),),
        "SPL Token's TokenError::InsufficientFunds (= Custom(1))",
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE
    );
}

#[test]
fn validate_and_prepare_sequential_nonce_increments() {
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = PrepareParams {
        input_amount: 100_000,
        output_amount: 99_500,
        target_token: fx.target_token,
        action_idxs: DEFAULT_ACTION_IDXS.to_vec(),
    };

    for expected_pre_nonce in 0u64..3 {
        let meta = fx
            .invoke(&mut svm, params.clone())
            .unwrap_or_else(|_| panic!("deposit {expected_pre_nonce} should succeed"));

        let ev = extract_deposit_prepared(&meta.logs).expect("event");
        assert_eq!(
            ev.nonce, expected_pre_nonce,
            "deposit #{expected_pre_nonce} carries pre-increment nonce",
        );
        let expected_msg = expected_canonical_message(&fx, expected_pre_nonce, &params);
        assert_eq!(
            ev.message, expected_msg,
            "deposit #{expected_pre_nonce} message must match byte-for-byte",
        );

        let n = fetch_mint_nonce(&svm, &fx.accounts.mint_nonce);
        assert_eq!(
            n.nonce,
            expected_pre_nonce + 1,
            "nonce advances after each deposit",
        );
    }

    // After 3 successful deposits: adapter_custody holds 3x 100_000, owner is short the same.
    assert_eq!(
        fetch_token_balance(&svm, &fx.accounts.adapter_custody),
        300_000
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE - 300_000
    );
}
