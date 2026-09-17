//! Destination E2E against the REAL bridge programs on a Surfpool mainnet
//! fork: the Across SVM Spoke Pool and the CCTP v2 MessageTransmitter /
//! TokenMessengerMinter.
//!
//! LiteSVM destination tests exercise every reject path against fabricated
//! accounts, but those are built from the same layout assumptions the
//! adapters themselves encode — for Across the `FillStatusAccount` layout /
//! seeds / relay-hash preimage, for CCTP the `used_nonce` seeds, transmitter
//! state layout, and message wire format. A drift in ANY of those passes the
//! fabrication and bricks production. Here the REAL programs create the
//! evidence accounts the adapters verify:
//!
//!   fill_relay / receive_message   (real program escrows into the route vault)
//!     → verify_*_ + finalize_fill  (one v0 tx, real ALT, 400k CU)
//!     → execute_actions            (happy path: committed 60/40 SplTransfer split)
//!     → recover_funds              (recovery path: owner drains execution vault)
//!     → settle again               (replay: rejected per evidence layer while
//!                                   the route vault is funded by a second leg)
//!
//! Every hop asserts EXACT balance conservation, with the amounts
//! deliberately de-aliased: unrelated dust escrowed in the route vault must
//! survive finalize untouched (finalize draws exactly the verified amount,
//! not the vault balance), and the 60/40 split gives each recipient a delta
//! distinct from every vault total. Across forwards the full fill amount (no
//! destination-side fees); CCTP delivers `amount − fee_executed`, with the
//! fee landing in Circle's fee-recipient ATA.
//!
//! The Across fill is sent with an EMPTY `message`: that is the production
//! shape for a vault recipient (the real program would otherwise CPI a
//! handler), and it pins the adapter's empty-message convention (zero
//! bytes32, not `keccak256("")`) against the real program's preimage.
//!
//! KEEPER SPEC (EVM → Solana over CCTP; this suite is the reference for the
//! on-chain leg): (1) observe the EVM `depositForBurn`; (2) poll Iris
//! `GET /v2/messages/{sourceDomain}?transactionHash=<tx>` until
//! `status == complete`; (3) submit `receive_message(message, attestation)`
//! ([`receive_message_ix`] is the reference account list) and, in the SAME
//! transaction where possible, `verify_cctp_mint` + `finalize_fill` — the
//! single-tx grouping closes the attester-rotation window; (4)
//! `execute_actions` as today. (The suite submits receive separately from
//! verify+finalize precisely to prove the crafted evidence stands on its own.)

mod common;

use common::cctp::{derive_cctp_verified_fill_pda, derive_used_nonce_pda, verify_cctp_mint_ix};
use common::dest_e2e::*;
use common::surfpool::*;
use common::*;
use solana_program::keccak;
use solana_signer::Signer;
use sra_common::constants::{
    ACROSS_SPOKE_POOL_ID, CCTP_MESSAGE_TRANSMITTER_V2_ID, CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
};
use sra_common::types::ExecutionStatus;

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_across_happy_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap(ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let relay_data = fx.relay_data().expect("build relay data");

    let pre_relayer = fx.token_amount(&fx.relayer_ata).expect("read relayer ATA");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        0,
        "fresh route vault must start empty"
    );
    for (label, ata) in [("A", &fx.recipient_a_ata), ("B", &fx.recipient_b_ata)] {
        assert_eq!(
            fx.token_amount(ata).expect("read recipient ATA"),
            0,
            "fresh action recipient {label} must start empty"
        );
    }

    let (_, logs) = fx
        .execute_real_fill(&relay_data)
        .expect("real fill_relay should succeed against real Across");
    assert_program_ran(
        &logs,
        "Across spoke pool (fill_relay + emit_cpi event)",
        &ACROSS_SPOKE_POOL_ID,
        2,
    );
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        OUTPUT_AMOUNT,
        "real fill must deliver the exact output amount to the route vault"
    );

    // Unrelated funds escrowed in the route vault (e.g. another fill awaiting
    // settlement) — finalize must move exactly the verified amount past them.
    fx.seed_route_vault_dust().expect("seed route vault dust");
    assert_eq!(
        fx.token_amount(&fx.relayer_ata).expect("read relayer ATA"),
        pre_relayer - OUTPUT_AMOUNT - ROUTE_VAULT_DUST
    );

    let fill_id = fx
        .verify_and_finalize(&relay_data)
        .expect("verify_across_fill + finalize_fill should succeed against the real FillStatus");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        ROUTE_VAULT_DUST,
        "finalize must draw exactly the verified amount — the unrelated vault balance stays"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT,
        "finalize must forward the FULL amount (no destination fees)"
    );
    let receipt = fx.fill_receipt(fill_id).expect("read fill receipt");
    assert_eq!(receipt.execution_status, ExecutionStatus::Finalized);
    assert_eq!(receipt.route, fx.route);
    assert_eq!(receipt.fill_id, fill_id);
    assert_eq!(
        receipt.amount, OUTPUT_AMOUNT,
        "the receipt must record the verified fill amount"
    );
    assert_eq!(receipt.expected_amount, OUTPUT_AMOUNT);

    fx.execute_actions(fill_id)
        .expect("execute_actions should run the committed SplTransfer split");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        0,
        "actions must drain the execution vault to exactly 0"
    );
    assert_eq!(
        fx.token_amount(&fx.recipient_a_ata)
            .expect("read recipient A ATA"),
        SPLIT_A_AMOUNT,
        "recipient A must receive exactly its committed 60% share"
    );
    assert_eq!(
        fx.token_amount(&fx.recipient_b_ata)
            .expect("read recipient B ATA"),
        SPLIT_B_AMOUNT,
        "recipient B must receive exactly its committed 40% share"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_across_recovery_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap(ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let relay_data = fx.relay_data().expect("build relay data");

    fx.execute_real_fill(&relay_data)
        .expect("real fill_relay should succeed against real Across");
    let fill_id = fx
        .verify_and_finalize(&relay_data)
        .expect("verify_across_fill + finalize_fill should succeed against the real FillStatus");

    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Finalized,
        "recovery must start from a Finalized receipt"
    );
    let pre_owner = fx.token_amount(&fx.owner_ata).expect("read owner ATA");
    fx.recover_funds(fill_id)
        .expect("owner should recover the finalized funds instead of executing actions");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        0,
        "recovery must drain the execution vault to exactly 0"
    );
    assert_eq!(
        fx.token_amount(&fx.owner_ata).expect("read owner ATA"),
        pre_owner + OUTPUT_AMOUNT,
        "recovery must pay the owner the exact fill amount"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Recovered
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_across_replay_rejected() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap(ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let relay_data = fx.relay_data().expect("build relay data");

    fx.execute_real_fill(&relay_data)
        .expect("real fill_relay should succeed against real Across");
    let fill_id = fx
        .verify_and_finalize(&relay_data)
        .expect("first finalize succeeds");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT
    );

    // A SECOND, distinct deposit is filled into the same route, so the route
    // vault holds ≥ the first fill's amount when its settlement is replayed —
    // if replay protection failed, the replay would actually have funds to
    // double-draw. (With an empty vault the replay could be rejected for the
    // wrong reason — insufficient balance — and prove nothing.)
    let second_relay_data = fx
        .relay_data_with(
            keccak::hash(&fx.route.to_bytes()).to_bytes(),
            SECOND_FILL_AMOUNT,
        )
        .expect("build second relay data");
    fx.execute_real_fill(&second_relay_data)
        .expect("second real fill_relay should succeed against real Across");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        SECOND_FILL_AMOUNT
    );

    // Replay the settle leg: the VerifiedFill evidence still exists on-chain,
    // but the persistent FillReceipt at ["fill_receipt", route, fill_id]
    // makes the second finalize's `init` fail — funds cannot be drawn twice
    // for one fill.
    let (verified_fill, _) = derive_verified_fill_pda(&fill_id);
    let replay = send_v1_transaction(
        &fx.rpc,
        "finalize_fill_replay",
        &fx.payer,
        &[],
        &[fx.finalize_ix(fill_id, &verified_fill)],
    );
    let replay_err = format!(
        "{}",
        replay.expect_err("second finalize_fill of the same fill must be rejected")
    );
    // The system program logs `Allocate: account <address> already in use`,
    // and the RPC error surfaces the simulation logs — pin BOTH the collision
    // and that the colliding account is the FillReceipt itself.
    let (fill_receipt_pda, _) = derive_fill_receipt_pda(&fx.route, &fill_id);
    assert!(
        replay_err.contains("already in use") && replay_err.contains(&fill_receipt_pda.to_string()),
        "replay must be rejected on the existing FillReceipt account {fill_receipt_pda} (init collision), got: {replay_err}"
    );

    // Rejection preserves state: the funded route vault is untouched (no
    // double draw), the execution vault holds only the first settlement, and
    // the receipt is still Finalized.
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        SECOND_FILL_AMOUNT,
        "rejected replay must not draw from the route vault"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT,
        "rejected replay must not move funds into the execution vault"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Finalized
    );

    // Post-rejection liveness: the rejected replay must not have wedged the
    // route — the second fill settles normally afterwards, drawing exactly
    // its own amount.
    let second_fill_id = fx
        .verify_and_finalize(&second_relay_data)
        .expect("second fill settles normally after the rejected replay");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        0,
        "settling the second fill must drain exactly its own escrow"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT + SECOND_FILL_AMOUNT
    );
    assert_eq!(
        fx.fill_receipt(second_fill_id)
            .expect("read second fill receipt")
            .execution_status,
        ExecutionStatus::Finalized
    );
}

/// The full CCTP keeper sequence with EXACT conservation at every hop,
/// parameterized over `fee_executed` (0 = standard transfer; > 0 = fast
/// transfer, where Circle's fee recipient collects the difference and the
/// route is settled for `amount − fee_executed`).
fn cctp_happy_path(fee_executed: u64) {
    let fx = SurfpoolDestFixture::bootstrap(CCTP_ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let delivered = OUTPUT_AMOUNT - fee_executed;
    let msg = fx
        .craft_attested_message(0, OUTPUT_AMOUNT, CCTP_MAX_FEE, fee_executed)
        .expect("craft attested message");

    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        0,
        "fresh route vault must start empty"
    );
    let pre_fee_recipient = fx
        .token_amount(&cctp_fee_recipient_ata())
        .expect("read fee-recipient ATA");

    let logs = fx
        .execute_real_receive(&msg)
        .expect("real receive_message should succeed against real CCTP v2");
    assert_program_ran(
        &logs,
        "MessageTransmitter",
        &CCTP_MESSAGE_TRANSMITTER_V2_ID,
        1,
    );
    assert_program_ran(
        &logs,
        "TokenMessengerMinter",
        &CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        1,
    );
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        delivered,
        "real mint must deliver exactly amount − fee_executed to the route vault"
    );
    assert_eq!(
        fx.token_amount(&cctp_fee_recipient_ata())
            .expect("read fee-recipient ATA"),
        pre_fee_recipient + fee_executed,
        "fee_executed must land in Circle's fee-recipient ATA"
    );

    // Unrelated funds escrowed in the route vault (e.g. another mint awaiting
    // settlement) — finalize must move exactly the verified amount past them.
    fx.seed_route_vault_dust().expect("seed route vault dust");

    let fill_id = fx
        .verify_and_finalize_cctp(&msg)
        .expect("verify_cctp_mint + finalize_fill should succeed against the real used_nonce");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        ROUTE_VAULT_DUST,
        "finalize must draw exactly the verified amount — the unrelated vault balance stays"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        delivered,
        "finalize must forward exactly the delivered amount"
    );
    let receipt = fx.fill_receipt(fill_id).expect("read fill receipt");
    assert_eq!(receipt.execution_status, ExecutionStatus::Finalized);
    assert_eq!(receipt.route, fx.route);
    assert_eq!(receipt.fill_id, fill_id);
    assert_eq!(
        receipt.amount, delivered,
        "the receipt must record the delivered amount"
    );
    assert_eq!(
        receipt.expected_amount,
        OUTPUT_AMOUNT - CCTP_MAX_FEE,
        "the receipt must record the source-committed worst-case quote"
    );

    fx.execute_actions(fill_id)
        .expect("execute_actions should run the committed SplTransfer split");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        0,
        "actions must drain the execution vault to exactly 0"
    );
    assert_eq!(
        fx.token_amount(&fx.recipient_a_ata)
            .expect("read recipient A ATA"),
        delivered * u64::from(SPLIT_A_BPS) / 10_000,
        "recipient A must receive exactly its committed 60% share of the delivered amount"
    );
    assert_eq!(
        fx.token_amount(&fx.recipient_b_ata)
            .expect("read recipient B ATA"),
        delivered * u64::from(SPLIT_B_BPS) / 10_000,
        "recipient B must receive exactly its committed 40% share of the delivered amount"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::ActionsSucceeded
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_cctp_happy_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    cctp_happy_path(0);
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_cctp_fee_executed_happy_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    cctp_happy_path(CCTP_FEE_EXECUTED);
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_cctp_recovery_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap(CCTP_ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let msg = fx
        .craft_attested_message(0, OUTPUT_AMOUNT, CCTP_MAX_FEE, 0)
        .expect("craft attested message");

    fx.execute_real_receive(&msg)
        .expect("real receive_message should succeed against real CCTP v2");
    let fill_id = fx
        .verify_and_finalize_cctp(&msg)
        .expect("verify_cctp_mint + finalize_fill should succeed against the real used_nonce");

    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Finalized,
        "recovery must start from a Finalized receipt"
    );
    let pre_owner = fx.token_amount(&fx.owner_ata).expect("read owner ATA");
    fx.recover_funds(fill_id)
        .expect("owner should recover the finalized funds instead of executing actions");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        0,
        "recovery must drain the execution vault to exactly 0"
    );
    assert_eq!(
        fx.token_amount(&fx.owner_ata).expect("read owner ATA"),
        pre_owner + OUTPUT_AMOUNT,
        "recovery must pay the owner the exact delivered amount"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Recovered
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_cctp_replay_rejected() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap(CCTP_ADAPTER_PROGRAM_ID).expect("bootstrap fixture");
    let msg = fx
        .craft_attested_message(0, OUTPUT_AMOUNT, CCTP_MAX_FEE, 0)
        .expect("craft attested message");

    fx.execute_real_receive(&msg)
        .expect("real receive_message should succeed against real CCTP v2");

    // Replay the mint leg: the same attested message a second time — the
    // transmitter's own `used_nonce` account makes the `init` collide.
    let receive_replay_err = format!(
        "{}",
        fx.execute_real_receive(&msg)
            .expect_err("second receive_message of the same message must be rejected")
    );
    let used_nonce = derive_used_nonce_pda(&msg.nonce);
    assert!(
        receive_replay_err.contains("already in use")
            && receive_replay_err.contains(&used_nonce.to_string()),
        "receive replay must be rejected on the existing used_nonce account {used_nonce} (init collision), got: {receive_replay_err}"
    );

    let fill_id = fx
        .verify_and_finalize_cctp(&msg)
        .expect("first settle succeeds");
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT
    );

    // A SECOND mint funds the same route, so the route vault holds ≥ the
    // first mint's amount when its settlement legs are replayed — a failed
    // replay guard would have real funds to double-draw. (With an empty vault
    // the replay could be rejected for the wrong reason — insufficient
    // balance — and prove nothing.)
    let second = fx
        .craft_attested_message(1, SECOND_FILL_AMOUNT, CCTP_MAX_FEE, 0)
        .expect("craft second attested message");
    fx.execute_real_receive(&second)
        .expect("second real receive_message should succeed");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        SECOND_FILL_AMOUNT
    );

    // Replay the verify leg: the evidence account is `init` — the existing
    // VerifiedFill for this nonce makes the second verification collide.
    let (verified_fill, _) = derive_cctp_verified_fill_pda(&msg.nonce);
    let verify_replay = send_v1_transaction(
        &fx.rpc,
        "verify_cctp_mint_replay",
        &fx.payer,
        &[],
        &[verify_cctp_mint_ix(
            &fx.payer.pubkey(),
            &fx.route,
            msg.nonce,
            msg.message.clone(),
            msg.attestation.clone(),
        )],
    );
    let verify_replay_err = format!(
        "{}",
        verify_replay.expect_err("second verify_cctp_mint of the same nonce must be rejected")
    );
    assert!(
        verify_replay_err.contains("already in use")
            && verify_replay_err.contains(&verified_fill.to_string()),
        "verify replay must be rejected on the existing VerifiedFill account {verified_fill} (init collision), got: {verify_replay_err}"
    );

    // Replay the settle leg: the VerifiedFill evidence still exists on-chain,
    // but the persistent FillReceipt at ["fill_receipt", route, fill_id]
    // makes the second finalize's `init` fail — funds cannot be drawn twice
    // for one mint.
    let replay = send_v1_transaction(
        &fx.rpc,
        "finalize_fill_replay",
        &fx.payer,
        &[],
        &[fx.finalize_ix(fill_id, &verified_fill)],
    );
    let replay_err = format!(
        "{}",
        replay.expect_err("second finalize_fill of the same fill must be rejected")
    );
    let (fill_receipt_pda, _) = derive_fill_receipt_pda(&fx.route, &fill_id);
    assert!(
        replay_err.contains("already in use") && replay_err.contains(&fill_receipt_pda.to_string()),
        "finalize replay must be rejected on the existing FillReceipt account {fill_receipt_pda} (init collision), got: {replay_err}"
    );

    // Rejection preserves state: the funded route vault is untouched (no
    // double draw), the execution vault holds only the first settlement, and
    // the receipt is still Finalized.
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        SECOND_FILL_AMOUNT,
        "rejected replays must not draw from the route vault"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT,
        "rejected replays must not move funds into the execution vault"
    );
    assert_eq!(
        fx.fill_receipt(fill_id)
            .expect("read fill receipt")
            .execution_status,
        ExecutionStatus::Finalized
    );

    // Post-rejection liveness: the rejected replays must not have wedged the
    // route — the second mint settles normally afterwards, drawing exactly
    // its own amount.
    let second_fill_id = fx
        .verify_and_finalize_cctp(&second)
        .expect("second mint settles normally after the rejected replays");
    assert_eq!(
        fx.token_amount(&fx.route_vault).expect("read route vault"),
        0,
        "settling the second mint must drain exactly its own escrow"
    );
    assert_eq!(
        fx.token_amount(&fx.execution_vault)
            .expect("read execution vault"),
        OUTPUT_AMOUNT + SECOND_FILL_AMOUNT
    );
    assert_eq!(
        fx.fill_receipt(second_fill_id)
            .expect("read second fill receipt")
            .execution_status,
        ExecutionStatus::Finalized
    );
}
