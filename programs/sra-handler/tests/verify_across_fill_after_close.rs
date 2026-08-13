//! Owner-attested recovery after Across reclaims a delivered fill's status PDA.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    clock::Clock,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::TransactionError,
};
use sra_across_adapter::utils::svm_spoke::types::RelayData;
use sra_common::types::ExecutionStatus;

const OUTPUT_AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;

fn anchor_error(code: ErrorCode) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(code as u32))
}

fn set_time(svm: &mut litesvm::LiteSVM, unix_timestamp: i64) {
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = unix_timestamp;
    svm.set_sysvar::<Clock>(&clock);
}

struct FilledRoute {
    payer: Keypair,
    owner: Keypair,
    relayer: Keypair,
    mint: Pubkey,
    route: Pubkey,
    route_config_hash: [u8; 32],
    route_vault: Pubkey,
    relay_data: RelayData,
    relay_hash: [u8; 32],
}

fn setup_filled_route(svm: &mut litesvm::LiteSVM) -> FilledRoute {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let relayer = funded_keypair(svm, 100 * SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let (route, route_config_hash) =
        create_sample_route_with_hash(svm, &payer, &owner, mint, [0x61; 32]);
    let route_vault = derive_ata(&route, &mint);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(svm, &payer, &payer, &mint, &relayer_ata, OUTPUT_AMOUNT);

    let fill_deadline = u32::try_from(now(svm) + 10).expect("LiteSVM timestamp fits u32");
    let mut relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    relay_data.fill_deadline = fill_deadline;
    relay_data.exclusivity_deadline = fill_deadline - 1;
    let relay_hash = relay_hash_for(&relay_data);

    send_ix(
        svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            OUTPUT_AMOUNT,
            fill_deadline,
        ),
        &relayer,
        &[],
    )
    .expect("fill_relay should deliver tokens and record Filled");

    FilledRoute {
        payer,
        owner,
        relayer,
        mint,
        route,
        route_config_hash,
        route_vault,
        relay_data,
        relay_hash,
    }
}

impl FilledRoute {
    fn advance_past_deadline_and_close(&self, svm: &mut litesvm::LiteSVM) {
        set_time(svm, i64::from(self.relay_data.fill_deadline) + 1);
        let fill_status = derive_fill_status_pda(&self.relay_hash).0;
        send_ix(
            svm,
            close_fill_pda_ix(&self.relayer.pubkey(), &fill_status),
            &self.relayer,
            &[],
        )
        .expect("recorded relayer closes status after deadline");
    }

    fn fallback_ix(&self) -> solana_sdk::instruction::Instruction {
        verify_across_fill_after_close_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.route,
            self.relay_hash,
            self.relay_data.clone(),
        )
    }

    fn finalize_ix(&self) -> solana_sdk::instruction::Instruction {
        let verified_fill = derive_verified_fill_pda(&self.relay_hash).0;
        finalize_fill_ix(
            &self.payer.pubkey(),
            &self.route,
            self.route_config_hash,
            &verified_fill,
            self.relay_hash,
            &self.mint,
        )
    }
}

#[test]
fn closed_fill_status_owner_fallback_finalizes_delivered_fill() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    assert_eq!(fetch_token_balance(&svm, &fix.route_vault), OUTPUT_AMOUNT);
    fix.advance_past_deadline_and_close(&mut svm);

    let normal = verify_across_fill_ix(
        &fix.payer.pubkey(),
        &fix.route,
        fix.relay_hash,
        fix.relay_data.clone(),
    );
    assert_eq!(
        send_ix_expect_err(&mut svm, normal, &fix.payer, &[], "closed status rejects"),
        anchor_error(ErrorCode::ConstraintOwner),
    );

    send_instructions(
        &mut svm,
        &[fix.fallback_ix(), fix.finalize_ix()],
        &fix.payer,
        &[&fix.owner],
    )
    .expect("owner fallback and normal finalization should compose");

    let execution_authority = derive_execution_authority_pda(&fix.route).0;
    let execution_vault = derive_ata(&execution_authority, &fix.mint);
    assert_eq!(fetch_token_balance(&svm, &fix.route_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), OUTPUT_AMOUNT);

    let receipt = fetch_fill_receipt(
        &svm,
        &derive_fill_receipt_pda(&fix.route, &fix.relay_hash).0,
    );
    assert_eq!(receipt.execution_status, ExecutionStatus::Finalized);
    assert_eq!(receipt.amount, OUTPUT_AMOUNT);
}
