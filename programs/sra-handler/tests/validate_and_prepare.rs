mod common;

use common::*;
use solana_sdk::{instruction::InstructionError, signer::Signer, transaction::TransactionError};
use solana_system_interface::program as system_program;
use sra_common::{constants::MAX_ACTION_IDXS, errors::SraError};
use sra_handler::instructions::source::validate_and_prepare::PrepareParams;

const INPUT: u64 = 1_000_000;
// 50 bps slippage tolerance -> min acceptable = 995_000 for INPUT = 1_000_000.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const DEFAULT_ACTION_IDXS: [u64; 2] = [3, 7];

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

    // DepositPrepared event emitted with raw bytes32 routing inputs.
    let ev = extract_deposit_prepared(&meta.logs).expect("DepositPrepared event must be emitted");
    assert_eq!(ev.sra_config, fx.sra_config);
    assert_eq!(ev.source_mint, fx.source_mint);
    assert_eq!(ev.adapter_program, fx.adapter_program);
    assert_eq!(ev.nonce, 0, "event carries the pre-increment nonce");
    assert_eq!(ev.input_amount, INPUT);
    assert_eq!(ev.output_amount, OUTPUT_WITHIN_SLIPPAGE);
    assert_eq!(ev.target_chain_id, fx.config.target_chain_id);
    assert_eq!(ev.target_handler, fx.config.target_handler);
    assert_eq!(ev.target_adapter, fx.config.target_adapter);
    assert_eq!(ev.recipient, fx.config.recipient);
    assert_eq!(ev.app_id, fx.config.app_id);
    assert_eq!(ev.target_token, params.target_token);
    assert_eq!(ev.action_idxs, params.action_idxs);
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
fn validate_and_prepare_non_executable_adapter_rejected() {
    let mut svm = setup_svm();
    let mut fx = ValidatePrepareFixture::new(&mut svm);
    let non_executable_adapter = funded_keypair(&mut svm, SOL);

    let ix = update_adapters_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.sra_config,
        vec![non_executable_adapter.pubkey()],
    );
    send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).expect("update adapters");

    fx.adapter_program = non_executable_adapter.pubkey();
    fx.accounts =
        ValidatePrepareAccounts::derive(&fx.sra_config, &fx.source_mint, &fx.adapter_program);

    // Skip the default 2-IX flow: IX 2 would dispatch to the non-executable
    // account and the runtime would reject the tx before IX 1's
    // `InvalidAdapterProgram` check fires.
    let err = fx
        .invoke_standalone(&mut svm, default_prepare_params(&fx))
        .expect_err("non-executable adapter must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidAdapterProgram.into()),
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
fn validate_and_prepare_standalone_rejected() {
    // IX 1 alone in a tx — `verify_adapter_ix`'s `get_instruction_relative(+1)`
    // returns no instruction; the wrapper maps that to
    // `InvalidFollowingInstruction`. Tokens must never move.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let err = fx
        .invoke_standalone(&mut svm, default_prepare_params(&fx))
        .expect_err("standalone IX 1 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidFollowingInstruction.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "standalone IX 1 must not move tokens",
    );
    // adapter_custody is `init_if_needed` — it would only be created mid-IX 1,
    // and only after `verify_adapter_ix` has already passed. With the
    // standalone rejection happening at Step 0c the custody account is never
    // initialized, so we don't read its balance here.
}

#[test]
fn validate_and_prepare_wrong_next_ix_adapter_rejected() {
    // IX 1 nominates the registered adapter, but IX 2 targets a different
    // program (system program). verify_adapter_ix sees the program-id
    // mismatch and rejects IX 1 before tokens move.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let address_book_entry = fx.address_book_entry(&params.target_token);
    let ix1 = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        params.clone(),
    );
    let ix2 = build_synthetic_adapter_ix(&system_program::ID, &fx.sra_config, &params);

    let err = send_two_ixes(&mut svm, &fx.payer, &fx.owner, ix1, ix2)
        .expect_err("wrong adapter program in IX 2 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidFollowingInstruction.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "tokens must not move when IX 2 nominates wrong adapter",
    );
}

#[test]
fn validate_and_prepare_wrong_next_ix_routing_rejected() {
    // IX 2 has the right program + discriminator + sra_config, but its
    // `target_token` differs from IX 1's. The routing byte-comparison must
    // catch this.
    let mut svm = setup_svm();
    let fx = ValidatePrepareFixture::new(&mut svm);

    let params = default_prepare_params(&fx);
    let address_book_entry = fx.address_book_entry(&params.target_token);
    let ix1 = validate_and_prepare_ix(
        &fx.payer.pubkey(),
        &fx.owner.pubkey(),
        &fx.accounts,
        &fx.owner_ata,
        &address_book_entry,
        params.clone(),
    );
    let mismatched = PrepareParams {
        target_token: [0xFF; 32],
        ..params.clone()
    };
    let ix2 = build_synthetic_adapter_ix(&fx.adapter_program, &fx.sra_config, &mismatched);

    let err = send_two_ixes(&mut svm, &fx.payer, &fx.owner, ix1, ix2)
        .expect_err("mismatched routing in IX 2 must reject")
        .err
        .clone();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(SraError::InvalidFollowingInstruction.into()),
        )
    );
    assert_eq!(
        fetch_token_balance(&svm, &fx.owner_ata),
        OWNER_STARTING_BALANCE,
        "tokens must not move when IX 2 routing differs",
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
        assert_eq!(ev.target_token, params.target_token);
        assert_eq!(ev.action_idxs, params.action_idxs);

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
