use sra_common::{
    instruction_data::{decode_anchor_instruction_data, encode_anchor_instruction_data},
    source_deposit::{AcrossDepositParams, PrepareParams},
};

fn evm_bytes32(byte: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].fill(byte);
    out
}

fn sample_prepare_params() -> PrepareParams {
    PrepareParams::new(
        7,
        9,
        42_161,
        evm_bytes32(0x11),
        evm_bytes32(0x22),
        evm_bytes32(0x33),
        [0x44u8; 32],
        evm_bytes32(0x55),
        vec![1, 2],
    )
}

#[test]
fn decode_prepare_params_round_trips_valid_data() {
    let params = sample_prepare_params();
    let data = encode_anchor_instruction_data("validate_and_prepare", &params);

    let decoded =
        decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &data).unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_across_deposit_params_round_trips_valid_data() {
    let prepare = sample_prepare_params();
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
    let params = sample_prepare_params();
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
