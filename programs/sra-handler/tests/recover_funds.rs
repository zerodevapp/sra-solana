//! Owner-gated destination fund recovery (`recover_funds`).
//!
//! Tests stage the receipt + vault via fixtures (a planted receipt + a direct
//! mint) rather than the full fill flow — the only way to reach the
//! `ActionsFailed` / `ActionsSucceeded` / `Recovered` states, which no
//! instruction produces yet. The genuine finalize path is covered in
//! `finalize_fill.rs`.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use sra_common::{
    errors::SraError,
    types::{ExecutionStatus, ManageRouteParams},
};

const AMOUNT: u64 = 4_000_000;
const FILL_ID: [u8; 32] = [0x55; 32];

/// A route with a planted receipt and a (possibly funded) execution vault.
struct Recoverable {
    owner: Keypair,
    mint: Pubkey,
    route: Pubkey,
    fill_id: [u8; 32],
    receipt: Pubkey,
    execution_vault: Pubkey,
    owner_ata: Pubkey,
}

/// Route + a receipt planted in `status` + the execution vault funded to
/// `vault_amount` + the owner's receiving ATA.
fn setup_recoverable(
    svm: &mut litesvm::LiteSVM,
    status: ExecutionStatus,
    vault_amount: u64,
) -> Recoverable {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(svm, &payer, &owner, mint, [0x32; 32]);

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);
    if vault_amount > 0 {
        mint_to(svm, &payer, &payer, &mint, &execution_vault, vault_amount);
    }

    let receipt = seed_fill_receipt_with_status(svm, &route, FILL_ID, AMOUNT, status);
    let owner_ata = create_ata(svm, &payer, &owner.pubkey(), &mint);

    Recoverable {
        owner,
        mint,
        route,
        fill_id: FILL_ID,
        receipt,
        execution_vault,
        owner_ata,
    }
}

impl Recoverable {
    fn recover_ix(&self) -> solana_instruction::Instruction {
        recover_funds_ix(
            &self.owner.pubkey(),
            &self.route,
            &self.fill_id,
            &self.mint,
            &self.owner_ata,
        )
    }

    /// Run recover_funds (owner signs and pays).
    fn recover(
        &self,
        svm: &mut litesvm::LiteSVM,
    ) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
        send_ix(svm, self.recover_ix(), &self.owner, &[])
    }

    fn recover_err(&self, svm: &mut litesvm::LiteSVM, msg: &str) -> TransactionError {
        send_ix_expect_err(svm, self.recover_ix(), &self.owner, &[], msg)
    }

    fn status(&self, svm: &litesvm::LiteSVM) -> ExecutionStatus {
        fetch_fill_receipt(svm, &self.receipt).execution_status
    }
}

// --------------------------------------------------------------------------
// happy paths: {Finalized, ActionsFailed} -> Recovered, full drain to owner
// --------------------------------------------------------------------------

#[test]
fn recover_from_finalized_transfers_to_owner() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    r.recover(&mut svm)
        .expect("recover from Finalized should succeed");

    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &r.owner_ata), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Recovered);
}

#[test]
fn recover_funds_succeeds_while_paused() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    // Pause the route; recover_funds must still work (it's not pause-gated).
    send_ix(
        &mut svm,
        manage_route_ix(
            &r.owner.pubkey(),
            &r.route,
            ManageRouteParams {
                pause: Some(true),
                adapters: None,
                executors: None,
            },
            &[],
        ),
        &r.owner,
        &[],
    )
    .expect("pause should succeed");

    r.recover(&mut svm)
        .expect("recover must succeed on a paused route");

    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &r.owner_ata), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Recovered);
}

#[test]
fn recover_from_actions_failed_transfers_to_owner() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::ActionsFailed, AMOUNT);

    r.recover(&mut svm)
        .expect("recover from ActionsFailed should succeed");

    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &r.owner_ata), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Recovered);
}

// --------------------------------------------------------------------------
// lifecycle gate: terminal states reject (and never move funds)
// --------------------------------------------------------------------------

#[test]
fn recover_from_actions_succeeded_rejected() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::ActionsSucceeded, AMOUNT);

    assert_eq!(
        r.recover_err(&mut svm, "ActionsSucceeded is terminal"),
        custom_error(SraError::InvalidExecutionStatus)
    );
    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::ActionsSucceeded);
}

#[test]
fn recover_from_recovered_rejected() {
    // Recovery replay: an already-Recovered receipt is rejected by the same gate.
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Recovered, 0);

    assert_eq!(
        r.recover_err(&mut svm, "Recovered is terminal"),
        custom_error(SraError::InvalidExecutionStatus)
    );
    assert_eq!(r.status(&svm), ExecutionStatus::Recovered);
}

// --------------------------------------------------------------------------
// authorization + payout pinning
// --------------------------------------------------------------------------

#[test]
fn recover_non_owner_rejected() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    // An impostor signing in the owner slot, with their own ATA.
    let attacker = funded_keypair(&mut svm, SOL);
    let attacker_ata = create_ata(&mut svm, &attacker, &attacker.pubkey(), &r.mint);
    let ix = recover_funds_ix(
        &attacker.pubkey(),
        &r.route,
        &r.fill_id,
        &r.mint,
        &attacker_ata,
    );

    assert_eq!(
        send_ix_expect_err(&mut svm, ix, &attacker, &[], "non-owner must reject"),
        anchor_error(ErrorCode::ConstraintHasOne)
    );
    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Finalized);
}

#[test]
fn recover_to_non_canonical_ata_rejected() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    // Right mint, wrong authority — rejected on the token-owner dimension.
    let stranger_ata = create_ata(&mut svm, &r.owner, &Pubkey::new_unique(), &r.mint);
    let ix = recover_funds_ix(
        &r.owner.pubkey(),
        &r.route,
        &r.fill_id,
        &r.mint,
        &stranger_ata,
    );

    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &r.owner,
            &[],
            "payout must be the owner's ATA"
        ),
        anchor_error(ErrorCode::ConstraintTokenOwner)
    );
    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Finalized);
}

#[test]
fn recover_to_wrong_mint_account_rejected() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    // Owner's ATA but for the wrong mint — rejected on the canonical-address dimension.
    let other_mint = create_mint(&mut svm, &r.owner, &r.owner.pubkey(), 6);
    let owner_other_ata = create_ata(&mut svm, &r.owner, &r.owner.pubkey(), &other_mint);
    let ix = recover_funds_ix(
        &r.owner.pubkey(),
        &r.route,
        &r.fill_id,
        &r.mint,
        &owner_other_ata,
    );

    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &r.owner,
            &[],
            "payout mint must be the route mint"
        ),
        anchor_error(ErrorCode::ConstraintAssociated)
    );
    assert_eq!(fetch_token_balance(&svm, &r.execution_vault), AMOUNT);
    assert_eq!(r.status(&svm), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// empty-vault guard
// --------------------------------------------------------------------------

#[test]
fn recover_empty_vault_rejected() {
    // Recoverable status, but an empty vault.
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, 0);

    assert_eq!(
        r.recover_err(&mut svm, "empty vault must reject"),
        custom_error(SraError::NothingToRecover)
    );
    assert_eq!(r.status(&svm), ExecutionStatus::Finalized);
}

// --------------------------------------------------------------------------
// replay-marker durability + event
// --------------------------------------------------------------------------

#[test]
fn recover_does_not_close_fill_receipt() {
    // The receipt must survive recovery — it is the replay marker.
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    r.recover(&mut svm).expect("recover should succeed");

    let acct = svm
        .get_account(&r.receipt)
        .expect("fill_receipt still exists after recovery");
    assert!(!acct.data.is_empty(), "receipt data is not zeroed/closed");
    assert_eq!(r.status(&svm), ExecutionStatus::Recovered);
}

#[test]
fn recover_emits_funds_recovered() {
    let mut svm = setup_svm();
    let r = setup_recoverable(&mut svm, ExecutionStatus::Finalized, AMOUNT);

    let meta =
        send_ix_with_meta(&mut svm, r.recover_ix(), &r.owner, &[]).expect("recover should succeed");
    let event: sra_handler::events::FundsRecovered =
        extract_anchor_event(&meta.logs).expect("FundsRecovered emitted");
    assert_eq!(event.route, r.route);
    assert_eq!(event.owner, r.owner.pubkey());
    assert_eq!(event.fill_id, r.fill_id);
    assert_eq!(event.amount, AMOUNT);
}
