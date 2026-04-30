use anchor_lang::AnchorSerialize;
use sra_common::{
    instruction_data::{decode_anchor_instruction_data, instruction_discriminator},
    source_deposit::{
        handler_output_amount_u256, AcrossDepositParams, NormalizedDepositIntent, PrepareParams,
    },
};

fn encode_ix_data<T: AnchorSerialize>(name: &str, value: &T) -> Vec<u8> {
    let mut data = instruction_discriminator(name).to_vec();
    value.serialize(&mut data).unwrap();
    data
}

#[test]
fn decode_prepare_params_round_trips_valid_data() {
    let params = PrepareParams {
        input_amount: 7,
        output_amount: 9,
        target_token: [4u8; 32],
        action_idxs: vec![1, 2],
    };
    let data = encode_ix_data("validate_and_prepare", &params);

    let decoded =
        decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &data).unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_across_deposit_params_round_trips_valid_data() {
    let params = AcrossDepositParams {
        input_amount: 7,
        output_amount: handler_output_amount_u256(9),
        target_token: [4u8; 32],
        action_idxs: vec![1, 2],
        exclusive_relayer: anchor_lang::prelude::Pubkey::new_from_array([6u8; 32]),
        fill_deadline_offset: 600,
        exclusivity_period: 120,
    };
    let data = encode_ix_data("execute_deposit_across", &params);

    let decoded =
        decode_anchor_instruction_data::<AcrossDepositParams>("execute_deposit_across", &data)
            .unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_prepare_params_rejects_wrong_discriminator() {
    let params = PrepareParams {
        input_amount: 7,
        output_amount: 9,
        target_token: [4u8; 32],
        action_idxs: vec![1, 2],
    };
    let data = encode_ix_data("wrong_name", &params);

    let err = decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &data)
        .expect_err("wrong discriminator must reject");
    assert!(format!("{err:?}").contains("InvalidPairedInstruction"));
}

#[test]
fn decode_across_deposit_params_rejects_truncated_data() {
    let err = decode_anchor_instruction_data::<AcrossDepositParams>(
        "execute_deposit_across",
        &[1, 2, 3, 4, 5, 6, 7, 8, 9],
    )
    .expect_err("truncated params must reject");
    assert!(format!("{err:?}").contains("InvalidPairedInstruction"));
}

#[test]
fn normalized_intent_matches_when_handler_output_is_promoted_to_u256() {
    let prepare = PrepareParams {
        input_amount: 11,
        output_amount: 22,
        target_token: [7u8; 32],
        action_idxs: vec![3, 5],
    };
    let across = AcrossDepositParams {
        input_amount: 11,
        output_amount: handler_output_amount_u256(22),
        target_token: [7u8; 32],
        action_idxs: vec![3, 5],
        exclusive_relayer: anchor_lang::prelude::Pubkey::new_from_array([1u8; 32]),
        fill_deadline_offset: 60,
        exclusivity_period: 30,
    };

    assert_eq!(
        NormalizedDepositIntent::from_prepare(&prepare),
        NormalizedDepositIntent::from_across(&across),
    );
}

#[test]
fn normalized_intent_differs_when_action_idxs_change() {
    let prepare = PrepareParams {
        input_amount: 11,
        output_amount: 22,
        target_token: [7u8; 32],
        action_idxs: vec![3, 5],
    };
    let across = AcrossDepositParams {
        input_amount: 11,
        output_amount: handler_output_amount_u256(22),
        target_token: [7u8; 32],
        action_idxs: vec![3, 6],
        exclusive_relayer: anchor_lang::prelude::Pubkey::new_from_array([1u8; 32]),
        fill_deadline_offset: 60,
        exclusivity_period: 30,
    };

    assert_ne!(
        NormalizedDepositIntent::from_prepare(&prepare),
        NormalizedDepositIntent::from_across(&across),
    );
}
