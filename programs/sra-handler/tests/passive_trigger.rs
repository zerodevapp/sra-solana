mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{pubkey::Pubkey, signer::Signer};
use sra_common::{
    abi::u256_from_u64,
    errors::SraError,
    source_deposit::{AcrossDepositParams, CctpDepositParams, PrepareParams},
};

#[test]
fn passive_sra_nonce_progresses_across_adapters_and_mints() {
    let mut svm = setup_svm();
    let cctp = PassiveFixture::new_cctp_with_adapter_policy(
        &mut svm,
        50,
        vec![CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID],
    );
    let cctp_params = sample_prepare_params(cctp.target_token);
    let (trigger, execute, event) = cctp_pair(
        &cctp,
        cctp_params.clone(),
        cctp_params_for(&cctp_params),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &cctp.payer,
        &[&cctp.initiator, &event],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &cctp.sra_config).deposit_nonce, 1);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 0);
    assert_eq!(
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        0
    );

    let across_mint = create_mint(&mut svm, &cctp.payer, &cctp.payer.pubkey(), 6);
    let across_vault = create_ata(&mut svm, &cctp.payer, &cctp.across_state, &across_mint);
    let across = PassiveFixture::for_mint(
        &mut svm,
        &cctp,
        across_mint,
        ADAPTER_PROGRAM_ID,
        cctp.across_state,
        across_vault,
    );
    let across_params = sample_prepare_params(across.target_token);
    let (trigger, execute) = across_pair(
        &across,
        across_params.clone(),
        AcrossDepositParams::from_prepare(&across_params, Pubkey::new_unique(), 600, 120).unwrap(),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &across.payer,
        &[&across.initiator],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &across.sra_config).deposit_nonce, 2);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 1);
    assert_eq!(
        extract_anchor_event::<sra_across_adapter::events::AcrossDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        1
    );

    let (trigger, execute, event) = cctp_pair(
        &cctp,
        cctp_params.clone(),
        cctp_params_for(&cctp_params),
        None,
    );
    let metadata = send_instructions(
        &mut svm,
        &[trigger, execute],
        &cctp.payer,
        &[&cctp.initiator, &event],
    )
    .unwrap();
    assert_eq!(fetch_sra_config(&svm, &cctp.sra_config).deposit_nonce, 3);
    assert_eq!(extract_deposit_triggered(&metadata.logs).unwrap().nonce, 2);
    assert_eq!(
        extract_anchor_event::<sra_cctp_adapter::events::CctpDepositExecuted>(&metadata.logs)
            .unwrap()
            .nonce,
        2
    );
}

#[test]
fn passive_trigger_rejects_wrong_missing_and_non_signing_initiator_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);

    let attacker = funded_keypair(&mut svm, SOL);
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[1].pubkey = attacker.pubkey();
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&attacker, &event],
        Some(event.pubkey()),
        "wrong initiator must reject",
    );
    assert_sra_error(error, 0, SraError::InitiatorNotAllowed);

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts.remove(1);
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&event],
        Some(event.pubkey()),
        "missing initiator must reject",
    );

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[1].is_signer = false;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&event],
        Some(event.pubkey()),
        "non-signing initiator must reject",
    );
    assert_eq!(error, anchor_error(ErrorCode::AccountNotSigner));
}

#[test]
fn passive_trigger_rejects_inactive_sra_and_replaced_initiator_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    send_ix(
        &mut svm,
        manage_sra_ix(
            fx.owner.pubkey(),
            fx.sra_config,
            fx.initiator.pubkey(),
            false,
        ),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap();
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "inactive SRA must reject",
    );
    assert_sra_error(error, 0, SraError::SraInactive);

    let replacement = funded_keypair(&mut svm, SOL);
    send_ix(
        &mut svm,
        manage_sra_ix(fx.owner.pubkey(), fx.sra_config, replacement.pubkey(), true),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap();
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "replaced initiator must reject",
    );
    assert_sra_error(error, 0, SraError::InitiatorNotAllowed);
}

#[test]
fn passive_trigger_pins_sra_mint_vault_authority_custody_and_adapter() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let other_mint = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 6);
    let substitutes = [
        (2, Pubkey::new_unique()),
        (3, other_mint),
        (4, fx.owner_ata),
        (5, Pubkey::new_unique()),
        (6, fx.owner_ata),
        (7, PROGRAM_ID),
    ];
    for (slot, substitute) in substitutes {
        let (mut trigger, execute, event) =
            cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
        trigger.accounts[slot].pubkey = substitute;
        assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "substituted trigger account must reject",
        );
    }
}

#[test]
fn passive_trigger_pins_address_book_and_rejects_disabled_entry() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = sample_prepare_params(fx.target_token);
    let wrong_entry = setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        10,
        fx.target_token,
        6,
    );
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts[8].pubkey = wrong_entry;
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong address-book entry must reject",
    );
    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    trigger.accounts.remove(8);
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "missing address-book entry must reject",
    );

    setup_address_book_entry(
        &mut svm,
        &fx.payer,
        &fx.owner,
        &fx.address_book_root,
        &fx.source_mint,
        fx.target_chain_id,
        fx.target_token,
        0,
    );
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "disabled address-book entry must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidAddressBook);
}

#[test]
fn passive_trigger_rejects_runtime_route_substitution_atomically() {
    type ParamsMutator = fn(&mut PrepareParams);
    let cases: [ParamsMutator; 3] = [
        |params| params.target_chain_id = 10,
        |params| params.recipient = evm_bytes32(0x99),
        |params| params.target_token = evm_bytes32(0x77),
    ];
    for mutate in cases {
        let mut svm = setup_svm();
        let fx = PassiveFixture::new(&mut svm);
        let mut params = sample_prepare_params(fx.target_token);
        mutate(&mut params);
        setup_address_book_entry(
            &mut svm,
            &fx.payer,
            &fx.owner,
            &fx.address_book_root,
            &fx.source_mint,
            params.target_chain_id,
            params.target_token,
            6,
        );
        let (trigger, execute, event) =
            cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
        let error = assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "runtime route substitution must reject",
        );
        assert_sra_error(error, 0, SraError::RouteMismatch);
    }
}

#[test]
fn passive_trigger_rejects_unallowlisted_and_non_executable_adapter_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new_with_adapter_policy(
        &mut svm,
        CCTP_ADAPTER_PROGRAM_ID,
        50,
        vec![ADAPTER_PROGRAM_ID],
    );
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unallowlisted adapter must reject",
    );
    assert_sra_error(error, 0, SraError::AdapterNotAllowed);

    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let mut program = svm.get_account(&CCTP_ADAPTER_PROGRAM_ID).unwrap();
    program.executable = false;
    svm.set_account(CCTP_ADAPTER_PROGRAM_ID, program).unwrap();
    let params = sample_prepare_params(fx.target_token);
    let (trigger, _, _) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger],
        &[&fx.initiator],
        None,
        "non-executable adapter must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidAdapterProgram);
}

#[test]
fn passive_trigger_rejects_zero_insufficient_and_exhausted_nonce_atomically() {
    let cases = [(0, 0), (SRA_STARTING_BALANCE + 1, SRA_STARTING_BALANCE)];
    for (input, output) in cases {
        let mut svm = setup_svm();
        let fx = PassiveFixture::new(&mut svm);
        let mut params = sample_prepare_params(fx.target_token);
        params.input_amount = input;
        params.output_amount = u256_from_u64(output);
        let cctp = CctpDepositParams::from_prepare(&params, 0, 2_000).unwrap();
        let (trigger, execute, event) = cctp_pair(&fx, params, cctp, None);
        let error = assert_rejected_unchanged(
            &mut svm,
            &fx,
            &[trigger, execute],
            &[&fx.initiator, &event],
            Some(event.pubkey()),
            "invalid amount must reject",
        );
        let expected = if input == 0 {
            SraError::ZeroInputAmount
        } else {
            SraError::InsufficientSourceBalance
        };
        assert_sra_error(error, 0, expected);
    }

    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    set_sra_config(&mut svm, &fx.sra_config, |state| {
        state.deposit_nonce = u64::MAX
    });
    let params = sample_prepare_params(fx.target_token);
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "exhausted nonce must reject",
    );
    assert_sra_error(error, 0, SraError::ArithmeticOverflow);
}

#[test]
fn passive_trigger_rejects_fee_policy_mismatches_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 2_999;
    params.output_amount = u256_from_u64(995_000);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong live fee must reject",
    );
    assert_sra_error(error, 0, SraError::FeeMismatch);

    params.protocol_fee_amount = 3_000;
    let (trigger, execute, event) = cctp_pair(&fx, params.clone(), cctp_params_for(&params), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "missing fee ATA must reject",
    );
    assert_sra_error(error, 0, SraError::MissingFeeRecipientAccount);

    let (mut trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    trigger.accounts[12].pubkey = fx.owner_ata;
    assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "wrong fee ATA must reject",
    );

    let mut sponsored = params.clone();
    sponsored.is_sponsor = true;
    sponsored.protocol_fee_amount = 0;
    let (trigger, execute, event) =
        cctp_pair(&fx, sponsored.clone(), cctp_params_for(&sponsored), None);
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unauthorized sponsorship must reject",
    );
    assert_sra_error(error, 0, SraError::SponsoredNotAllowed);

    let mut overridden = params.clone();
    overridden.protocol_fee_bps_override = 50;
    overridden.protocol_fee_amount = 5_000;
    let (trigger, execute, event) = cctp_pair(
        &fx,
        overridden.clone(),
        cctp_params_for(&overridden),
        Some(fee_ata),
    );
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "unauthorized override must reject",
    );
    assert_sra_error(error, 0, SraError::FeeMismatch);
}

#[test]
fn passive_trigger_rejects_slippage_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 3_000;
    params.output_amount = u256_from_u64(994_999);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "output below gross slippage floor must reject",
    );
    assert_sra_error(error, 0, SraError::SlippageExceeded);
}

#[test]
fn passive_trigger_rejects_pairing_mismatches_atomically() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let fee_ata = enable_protocol_fee(&mut svm, &fx, 30, false, false);
    let mut params = sample_prepare_params(fx.target_token);
    params.protocol_fee_amount = 3_000;
    params.output_amount = u256_from_u64(995_000);
    let (trigger, execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger],
        &[&fx.initiator],
        None,
        "missing adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::MissingPairedInstruction);

    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[
            trigger_ix(
                &fx.payer.pubkey(),
                &fx.initiator.pubkey(),
                &fx.accounts,
                &fx.address_book_entry(fx.target_chain_id, &fx.target_token),
                Some(fee_ata),
                params.clone(),
                &execute,
            ),
            compute_budget_ix(),
            execute,
        ],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "non-adjacent adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::InvalidPairedInstruction);

    let (trigger, mut execute, event) =
        cctp_pair(&fx, params.clone(), cctp_params_for(&params), Some(fee_ata));
    *execute.data.last_mut().unwrap() ^= 1;
    let error = assert_rejected_unchanged(
        &mut svm,
        &fx,
        &[trigger, execute],
        &[&fx.initiator, &event],
        Some(event.pubkey()),
        "data-mismatched adapter instruction must reject",
    );
    assert_sra_error(error, 0, SraError::PairedInstructionMismatch);
}
