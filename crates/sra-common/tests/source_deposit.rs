use sra_common::{
    instruction_data::{decode_anchor_instruction_data, encode_anchor_instruction_data},
    source_deposit::{AcrossDepositParams, PrepareParams, TriggerArgs},
};

fn evm_bytes32(byte: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].fill(byte);
    out
}

fn sample_prepare_params() -> PrepareParams {
    PrepareParams::new(
        7,
        sra_common::abi::u256_from_u64(9),
        42_161,
        evm_bytes32(0x33),
        evm_bytes32(0x55),
        1,
        false,
        sra_common::constants::NO_FEE_BPS_OVERRIDE,
    )
}

#[test]
fn decode_trigger_args_round_trips_valid_data() {
    let params = sample_prepare_params();
    let args = TriggerArgs {
        params: params.clone(),
        paired_adapter_ix_data_hash: [0xA5; 32],
    };
    let data = encode_anchor_instruction_data("trigger", &args);

    let decoded = decode_anchor_instruction_data::<TriggerArgs>("trigger", &data).unwrap();
    assert_eq!(decoded.params, params);
    assert_eq!(decoded.paired_adapter_ix_data_hash, [0xA5; 32]);
}

#[test]
fn decode_across_deposit_params_round_trips_valid_data() {
    let prepare = sample_prepare_params();
    let params = AcrossDepositParams::from_prepare(
        &prepare,
        anchor_lang::prelude::Pubkey::new_from_array([6u8; 32]),
        600,
        120,
    )
    .expect("sample params must produce a valid net amount");
    let data = encode_anchor_instruction_data("execute_deposit_across", &params);

    let decoded =
        decode_anchor_instruction_data::<AcrossDepositParams>("execute_deposit_across", &data)
            .unwrap();
    assert_eq!(decoded, params);
}

#[test]
fn decode_trigger_args_rejects_wrong_discriminator() {
    let args = TriggerArgs {
        params: sample_prepare_params(),
        paired_adapter_ix_data_hash: [0xA5; 32],
    };
    let data = encode_anchor_instruction_data("wrong_name", &args);

    let err = match decode_anchor_instruction_data::<TriggerArgs>("trigger", &data) {
        Ok(_) => panic!("wrong discriminator must reject"),
        Err(err) => err,
    };
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
