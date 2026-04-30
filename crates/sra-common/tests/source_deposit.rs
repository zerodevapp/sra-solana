use sra_common::{
    instruction_data::{decode_anchor_instruction_data, encode_anchor_instruction_data},
    source_deposit::{AcrossDepositParams, NormalizedDepositIntent, PrepareParams},
};

#[test]
fn decode_prepare_params_round_trips_valid_data() {
    let params = PrepareParams::new(7, 9, [4u8; 32], vec![1, 2]);
    let data = encode_anchor_instruction_data("validate_and_prepare", &params);

    let decoded =
        decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &data).unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_across_deposit_params_round_trips_valid_data() {
    let prepare = PrepareParams::new(7, 9, [4u8; 32], vec![1, 2]);
    let params = AcrossDepositParams::from_prepare(
        &prepare,
        anchor_lang::prelude::Pubkey::new_from_array([6u8; 32]),
        600,
        120,
    );
    let data = encode_anchor_instruction_data("execute_deposit_across", &params);

    let decoded =
        decode_anchor_instruction_data::<AcrossDepositParams>("execute_deposit_across", &data)
            .unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_prepare_params_rejects_wrong_discriminator() {
    let params = PrepareParams::new(7, 9, [4u8; 32], vec![1, 2]);
    let data = encode_anchor_instruction_data("wrong_name", &params);

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
    let prepare = PrepareParams::new(11, 22, [7u8; 32], vec![3, 5]);
    let across = AcrossDepositParams::from_prepare(
        &prepare,
        anchor_lang::prelude::Pubkey::new_from_array([1u8; 32]),
        60,
        30,
    );

    assert_eq!(
        NormalizedDepositIntent::from_prepare(&prepare),
        NormalizedDepositIntent::from_across(&across),
    );
}

#[test]
fn normalized_intent_differs_when_action_idxs_change() {
    let prepare = PrepareParams::new(11, 22, [7u8; 32], vec![3, 5]);
    let mut across = AcrossDepositParams::from_prepare(
        &prepare,
        anchor_lang::prelude::Pubkey::new_from_array([1u8; 32]),
        60,
        30,
    );
    across.action_idxs[1] += 1;

    assert_ne!(
        NormalizedDepositIntent::from_prepare(&prepare),
        NormalizedDepositIntent::from_across(&across),
    );
}
