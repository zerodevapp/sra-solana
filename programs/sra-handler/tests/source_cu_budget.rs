mod common;

use common::*;
use solana_sdk::{compute_budget::ComputeBudgetInstruction, pubkey::Pubkey};
use sra_common::{abi::u256_from_u64, source_deposit::AcrossDepositParams};

#[test]
fn passive_triggers_stay_under_source_cu_budget() {
    for adapter in [CCTP_ADAPTER_PROGRAM_ID, ADAPTER_PROGRAM_ID] {
        for fee_bps in [0, 30] {
            let mut svm = setup_svm();
            let fx = if adapter == CCTP_ADAPTER_PROGRAM_ID {
                PassiveFixture::new(&mut svm)
            } else {
                PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![adapter])
            };
            let fee_ata =
                (fee_bps != 0).then(|| enable_protocol_fee(&mut svm, &fx, fee_bps, false, false));
            let mut params = sample_prepare_params(fx.target_token);
            params.protocol_fee_amount = params.input_amount * u64::from(fee_bps) / 10_000;
            params.output_amount = u256_from_u64(995_000);
            let (trigger, execute, event) = if adapter == CCTP_ADAPTER_PROGRAM_ID {
                let (trigger, execute, event) =
                    cctp_pair(&fx, params.clone(), cctp_params_for(&params), fee_ata);
                (trigger, execute, Some(event))
            } else {
                let across =
                    AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120)
                        .expect("valid Across deposit params");
                let (trigger, execute) = across_pair(&fx, params, across, fee_ata);
                (trigger, execute, None)
            };
            let mut signers = vec![&fx.initiator];
            signers.extend(event.as_ref());
            let consumed = send_instructions(
                &mut svm,
                &[
                    ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                    trigger,
                    execute,
                ],
                &fx.payer,
                &signers,
            )
            .expect("passive source flow must execute")
            .compute_units_consumed;
            println!("passive source CU: adapter={adapter}, fee_bps={fee_bps}, consumed={consumed}, budget={SOURCE_CU_BUDGET}");
            assert!(
                consumed < u64::from(SOURCE_CU_BUDGET),
                "adapter={adapter}, fee_bps={fee_bps}: {consumed} CU exceeds budget"
            );
        }
    }
}
