//! Source compute-unit regression assertions for native-v1 transactions with
//! inline addresses and explicit `TransactionConfig` resource limits.

mod common;

use common::*;
use litesvm::LiteSVM;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use sra_common::source_deposit::AcrossDepositParams;

fn assert_under_budget(
    label: &str,
    fee_bps: u16,
    svm: &mut LiteSVM,
    fx: &PassiveFixture,
    [trigger, execute]: [Instruction; 2],
    signers: &[&Keypair],
) {
    let consumed = send_instructions_with_config(
        svm,
        &[trigger, execute],
        &fx.payer,
        signers,
        v1_config_with_compute_limit(1_400_000),
    )
    .expect("passive source flow must execute")
    .compute_units_consumed;
    let headroom = i128::from(SOURCE_CU_BUDGET) - i128::from(consumed);
    println!("passive {label} CU: fee_bps={fee_bps}, consumed={consumed}, budget={SOURCE_CU_BUDGET}, headroom={headroom}");
    assert!(
        consumed < u64::from(SOURCE_CU_BUDGET),
        "{label}, fee_bps={fee_bps}: {consumed} CU exceeds budget"
    );
}

#[test]
fn passive_cctp_stays_under_source_cu_budget() {
    for (fee_bps, fee_amount) in [(0, 0), (30, 3_000)] {
        let mut svm = setup_svm();
        let fx = PassiveFixture::new(&mut svm);
        let fee_ata =
            (fee_bps != 0).then(|| enable_protocol_fee(&mut svm, &fx, fee_bps, false, false));
        let params = fee_params(&fx, fee_amount);
        let (trigger, execute, event) =
            cctp_pair(&fx, params.clone(), cctp_params_for(&params), fee_ata);
        assert_under_budget(
            "CCTP",
            fee_bps,
            &mut svm,
            &fx,
            [trigger, execute],
            &[&fx.initiator, &event],
        );
    }
}

#[test]
fn passive_across_stays_under_source_cu_budget() {
    for (fee_bps, fee_amount) in [(0, 0), (30, 3_000)] {
        let mut svm = setup_svm();
        let fx =
            PassiveFixture::new_across_with_adapter_policy(&mut svm, 50, vec![ADAPTER_PROGRAM_ID]);
        let fee_ata =
            (fee_bps != 0).then(|| enable_protocol_fee(&mut svm, &fx, fee_bps, false, false));
        let params = fee_params(&fx, fee_amount);
        let across = AcrossDepositParams::from_prepare(&params, Pubkey::new_unique(), 600, 120)
            .expect("valid Across deposit params");
        let (trigger, execute) = across_pair(&fx, params, across, fee_ata);
        assert_under_budget(
            "Across",
            fee_bps,
            &mut svm,
            &fx,
            [trigger, execute],
            &[&fx.initiator],
        );
    }
}
