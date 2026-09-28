mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use sra_across_adapter::{utils::svm_spoke::types::RelayData, AcrossAdapterError};
use sra_common::types::ExecutionStatus;

const OUTPUT_AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;

fn set_fill_status_account(
    svm: &mut litesvm::LiteSVM,
    address: Pubkey,
    owner: Pubkey,
    data: Vec<u8>,
    lamports: u64,
) {
    svm.set_account(
        address,
        solana_account::Account {
            lamports,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("set fill-status test account");
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
    setup_filled_route_with_exclusivity_gap(svm, 0)
}

fn setup_filled_route_with_exclusivity_gap(
    svm: &mut litesvm::LiteSVM,
    exclusivity_gap: u32,
) -> FilledRoute {
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
    relay_data.exclusivity_deadline = fill_deadline
        .checked_sub(exclusivity_gap)
        .expect("exclusivity gap must not exceed fill deadline");
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

    fn fallback_ix(&self) -> solana_instruction::Instruction {
        verify_across_fill_after_close_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.route,
            self.relay_hash,
            self.relay_data.clone(),
        )
    }

    fn finalize_ix(&self) -> solana_instruction::Instruction {
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
#[should_panic(expected = "exclusivity gap must not exceed fill deadline")]
fn setup_rejects_exclusivity_gap_larger_than_fill_deadline() {
    let mut svm = setup_svm();
    setup_filled_route_with_exclusivity_gap(&mut svm, u32::MAX);
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

    let meta = send_instructions(
        &mut svm,
        &[fix.fallback_ix(), fix.finalize_ix()],
        &fix.payer,
        &[&fix.owner],
    )
    .expect("owner fallback and normal finalization should compose");

    let event: sra_across_adapter::events::AcrossFillVerifiedAfterClose =
        extract_anchor_event(&meta.logs).expect("fallback event emitted");
    assert_eq!(event.owner, fix.owner.pubkey());
    assert_eq!(event.route, fix.route);
    assert_eq!(event.fill_id, fix.relay_hash);
    assert_eq!(event.amount, OUTPUT_AMOUNT);

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

#[test]
fn fallback_rejects_fill_with_ambiguous_executed_amount() {
    let mut svm = setup_svm();
    let fix = setup_filled_route_with_exclusivity_gap(&mut svm, 1);
    fix.advance_past_deadline_and_close(&mut svm);

    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            fix.fallback_ix(),
            &fix.payer,
            &[&fix.owner],
            "ambiguous fill amount must reject",
        ),
        adapter_error(AcrossAdapterError::ExecutedAmountUnavailable)
    );
    assert!(svm
        .get_account(&derive_verified_fill_pda(&fix.relay_hash).0)
        .is_none());
}

#[test]
fn fallback_requires_the_route_owner() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    fix.advance_past_deadline_and_close(&mut svm);
    let stranger = funded_keypair(&mut svm, SOL);
    let ix = verify_across_fill_after_close_ix(
        &fix.payer.pubkey(),
        &stranger.pubkey(),
        &fix.route,
        fix.relay_hash,
        fix.relay_data.clone(),
    );
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &fix.payer,
            &[&stranger],
            "wrong owner rejects"
        ),
        anchor_error(ErrorCode::ConstraintHasOne),
    );
}

#[test]
fn fallback_rejects_a_live_across_owned_status() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    set_time(&mut svm, i64::from(fix.relay_data.fill_deadline) + 1);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            fix.fallback_ix(),
            &fix.payer,
            &[&fix.owner],
            "live status rejects",
        ),
        adapter_error(AcrossAdapterError::FillStatusNotClosed),
    );
}

#[test]
fn fallback_rejects_a_closed_shape_before_or_at_deadline() {
    for timestamp_offset in [-1i64, 0] {
        let mut svm = setup_svm();
        let fix = setup_filled_route(&mut svm);
        let status = derive_fill_status_pda(&fix.relay_hash).0;
        set_fill_status_account(
            &mut svm,
            status,
            solana_system_interface::program::ID,
            vec![],
            0,
        );
        set_time(
            &mut svm,
            i64::from(fix.relay_data.fill_deadline) + timestamp_offset,
        );
        assert_eq!(
            send_ix_expect_err(
                &mut svm,
                fix.fallback_ix(),
                &fix.payer,
                &[&fix.owner],
                "deadline has not strictly passed",
            ),
            adapter_error(AcrossAdapterError::FillDeadlineNotPassed),
        );
    }
}

#[test]
fn lamport_dust_on_closed_status_cannot_block_fallback() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    fix.advance_past_deadline_and_close(&mut svm);
    let status = derive_fill_status_pda(&fix.relay_hash).0;
    send_ix(
        &mut svm,
        solana_system_interface::instruction::transfer(&fix.payer.pubkey(), &status, 1_000_000),
        &fix.payer,
        &[],
    )
    .expect("dust closed status address");
    let account = svm.get_account(&status).expect("dusted status exists");
    assert_eq!(account.owner, solana_system_interface::program::ID);
    assert!(account.data.is_empty());

    send_ix(&mut svm, fix.fallback_ix(), &fix.payer, &[&fix.owner])
        .expect("lamports are irrelevant to closed-account detection");
}

#[test]
fn fallback_rejects_nonempty_or_foreign_owned_status_accounts() {
    for (owner, data) in [
        (solana_system_interface::program::ID, vec![1u8]),
        (PROGRAM_ID, Vec::new()),
    ] {
        let mut svm = setup_svm();
        let fix = setup_filled_route(&mut svm);
        let status = derive_fill_status_pda(&fix.relay_hash).0;
        set_fill_status_account(&mut svm, status, owner, data, 1);
        set_time(&mut svm, i64::from(fix.relay_data.fill_deadline) + 1);
        assert_eq!(
            send_ix_expect_err(
                &mut svm,
                fix.fallback_ix(),
                &fix.payer,
                &[&fix.owner],
                "not a closed Across status",
            ),
            adapter_error(AcrossAdapterError::FillStatusNotClosed),
        );
    }
}

#[test]
fn fallback_rejects_a_noncanonical_status_address() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    fix.advance_past_deadline_and_close(&mut svm);
    let canonical = derive_fill_status_pda(&fix.relay_hash).0;
    let mut ix = fix.fallback_ix();
    let slot = ix
        .accounts
        .iter()
        .position(|meta| meta.pubkey == canonical)
        .expect("fill-status meta");
    ix.accounts[slot].pubkey = Pubkey::new_unique();
    assert_eq!(
        send_ix_expect_err(&mut svm, ix, &fix.payer, &[&fix.owner], "wrong PDA rejects"),
        anchor_error(ErrorCode::ConstraintSeeds),
    );
}

#[test]
fn fallback_rejects_tampered_relay_data() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    fix.advance_past_deadline_and_close(&mut svm);
    let mut tampered = fix.relay_data.clone();
    tampered.output_amount += 1;
    let ix = verify_across_fill_after_close_ix(
        &fix.payer.pubkey(),
        &fix.owner.pubkey(),
        &fix.route,
        fix.relay_hash,
        tampered,
    );
    assert_eq!(
        send_ix_expect_err(&mut svm, ix, &fix.payer, &[&fix.owner], "tamper rejects"),
        adapter_error(AcrossAdapterError::RelayHashMismatch),
    );
}

#[test]
fn normal_verification_prevents_later_fallback_replay() {
    let mut svm = setup_svm();
    let fix = setup_filled_route(&mut svm);
    send_ix(
        &mut svm,
        verify_across_fill_ix(
            &fix.payer.pubkey(),
            &fix.route,
            fix.relay_hash,
            fix.relay_data.clone(),
        ),
        &fix.payer,
        &[],
    )
    .expect("normal verification wins");
    fix.advance_past_deadline_and_close(&mut svm);
    send_ix(&mut svm, fix.fallback_ix(), &fix.payer, &[&fix.owner])
        .expect_err("canonical VerifiedFill init prevents fallback replay");
}
