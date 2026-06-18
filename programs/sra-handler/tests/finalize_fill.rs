//! Destination fill finalization (`finalize_fill`).
//!
//! Consumes the adapter-written `VerifiedFill`, moves the **full** verified
//! amount from the route vault into the execution vault (no destination fees —
//! they were collected on the source), and writes the `FillReceipt` replay
//! marker in the `Finalized` state. This is the Solana analog of the EVM
//! `_triggerTargetAction` (its action-running half is a separate instruction).
//!
//! The genuine end-to-end tests fund the route vault with a real relayer fill
//! and run the real adapter `verify_across_fill`, so the cross-program
//! `VerifiedFill` ABI is exercised. The forgery / edge tests fabricate
//! adapter-owned evidence directly to probe each handler-side cross-check.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use solana_sdk::{
    clock::Clock, instruction::InstructionError, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::TransactionError,
};
use sra_common::{
    errors::SraError,
    types::{ExecutionStatus, ManageRouteParams},
};

const AMOUNT: u64 = 4_000_000;
/// Matches `sample_create_route_params` — the source chain the sample route is
/// bound to.
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_DEADLINE: u32 = 1_900_000_000;

fn custom_error(err: SraError) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(err.into()))
}

fn anchor_error(code: ErrorCode) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(code as u32))
}

// --------------------------------------------------------------------------
// genuine end-to-end evidence
// --------------------------------------------------------------------------

struct Finalizable {
    payer: Keypair,
    owner: Keypair,
    mint: Pubkey,
    route: Pubkey,
    route_config_hash: [u8; 32],
    fill_id: [u8; 32],
    verified_fill: Pubkey,
    route_vault: Pubkey,
    execution_vault: Pubkey,
}

/// Create the route, have a relayer fill the route vault exactly as the deployed
/// Across program does, then run the real adapter `verify_across_fill` to write
/// the `VerifiedFill`. Leaves a fill ready to finalize.
fn setup_finalizable_fill(svm: &mut litesvm::LiteSVM) -> Finalizable {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let (route, route_config_hash) =
        create_sample_route_with_hash(svm, &payer, &owner, mint, [0x30; 32]);

    let relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let route_vault = derive_ata(&route, &mint);

    let relayer = funded_keypair(svm, 100 * SOL);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(svm, &payer, &payer, &mint, &relayer_ata, AMOUNT);
    send_ix(
        svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            AMOUNT,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .expect("fill_relay should succeed");
    send_ix(
        svm,
        verify_across_fill_ix(&payer.pubkey(), &route, relay_hash, relay_data),
        &payer,
        &[],
    )
    .expect("verify_across_fill should succeed");

    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);

    Finalizable {
        payer,
        owner,
        mint,
        route,
        route_config_hash,
        fill_id: relay_hash,
        verified_fill,
        route_vault,
        execution_vault,
    }
}

impl Finalizable {
    fn finalize_ix(&self) -> solana_sdk::instruction::Instruction {
        finalize_fill_ix(
            &self.payer.pubkey(),
            &self.route,
            self.route_config_hash,
            &self.verified_fill,
            self.fill_id,
            &self.mint,
        )
    }

    fn receipt_pda(&self) -> Pubkey {
        derive_fill_receipt_pda(&self.route, &self.fill_id).0
    }

    /// Run finalize_fill from the (permissionless) payer.
    fn finalize(
        &self,
        svm: &mut litesvm::LiteSVM,
    ) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
        send_ix(svm, self.finalize_ix(), &self.payer, &[])
    }

    /// Run finalize_fill and return the rejection error.
    fn finalize_err(&self, svm: &mut litesvm::LiteSVM, msg: &str) -> TransactionError {
        send_ix_expect_err(svm, self.finalize_ix(), &self.payer, &[], msg)
    }

    /// Owner-gated pause toggle via manage_route.
    fn set_paused(&self, svm: &mut litesvm::LiteSVM, paused: bool) {
        self.manage(
            svm,
            ManageRouteParams {
                pause: Some(paused),
                adapters: None,
            },
            &[],
        );
    }

    /// Owner-gated allowlist replace via manage_route (remaining accounts mirror the set).
    fn set_adapters(&self, svm: &mut litesvm::LiteSVM, adapters: &[Pubkey]) {
        self.manage(
            svm,
            ManageRouteParams {
                pause: None,
                adapters: Some(adapters.to_vec()),
            },
            adapters,
        );
    }

    fn manage(&self, svm: &mut litesvm::LiteSVM, params: ManageRouteParams, adapters: &[Pubkey]) {
        send_ix(
            svm,
            manage_route_ix(&self.owner.pubkey(), &self.route, params, adapters),
            &self.payer,
            &[&self.owner],
        )
        .expect("manage_route should succeed");
    }
}

// --------------------------------------------------------------------------
// happy path
// --------------------------------------------------------------------------

#[test]
fn finalize_fill_happy_path() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    f.finalize(&mut svm).expect("finalize_fill should succeed");

    // The full verified amount moved route_vault -> execution_vault (no fees).
    assert_eq!(fetch_token_balance(&svm, &f.route_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);

    let (receipt_pda, bump) = derive_fill_receipt_pda(&f.route, &f.fill_id);
    let receipt = fetch_fill_receipt(&svm, &receipt_pda);
    assert_eq!(receipt.route, f.route);
    assert_eq!(receipt.fill_id, f.fill_id);
    assert_eq!(receipt.amount, AMOUNT);
    assert_eq!(receipt.expected_amount, AMOUNT);
    assert_eq!(receipt.execution_status, ExecutionStatus::Finalized);
    assert_eq!(
        receipt.finalized_at,
        svm.get_sysvar::<Clock>().unix_timestamp
    );
    assert_eq!(receipt.bump, bump);

    // finalize_fill records the receipt but never closes the VerifiedFill — the
    // FillReceipt is the replay marker, so re-creating the VerifiedFill later is
    // harmless.
    assert!(svm.get_account(&f.verified_fill).is_some());
}

#[test]
fn finalize_fill_forwards_full_amount() {
    // Conservation pin: the execution vault grows by exactly the verified amount
    // and the route vault drains by exactly the same — there is no fee leg on
    // the destination (fees were collected on the source deposit).
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    let route_before = fetch_token_balance(&svm, &f.route_vault);
    let exec_before = fetch_token_balance(&svm, &f.execution_vault);
    assert_eq!(route_before, AMOUNT);
    assert_eq!(exec_before, 0);

    f.finalize(&mut svm).expect("finalize_fill should succeed");

    assert_eq!(
        fetch_token_balance(&svm, &f.execution_vault) - exec_before,
        AMOUNT
    );
    assert_eq!(
        route_before - fetch_token_balance(&svm, &f.route_vault),
        AMOUNT
    );
}

#[test]
fn finalize_fill_emits_fill_finalized() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    let meta = send_ix_with_meta(&mut svm, f.finalize_ix(), &f.payer, &[])
        .expect("finalize_fill should succeed");
    let event: sra_handler::events::FillFinalized =
        extract_anchor_event(&meta.logs).expect("FillFinalized emitted");
    assert_eq!(event.route, f.route);
    assert_eq!(event.fill_id, f.fill_id);
    assert_eq!(event.amount, AMOUNT);
    assert_eq!(event.expected_amount, AMOUNT);
    assert_eq!(event.execution_status, ExecutionStatus::Finalized);
    assert_eq!(event.finalized_at, svm.get_sysvar::<Clock>().unix_timestamp);
}

// --------------------------------------------------------------------------
// pause gate (rejection must preserve state)
// --------------------------------------------------------------------------

#[test]
fn finalize_fill_route_paused_rejected() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    f.set_paused(&mut svm, true);

    assert_eq!(
        f.finalize_err(&mut svm, "paused must reject"),
        custom_error(SraError::RoutePaused)
    );
    // The fill is untouched: no receipt, no fund movement.
    assert!(svm.get_account(&f.receipt_pda()).is_none());
    assert_eq!(fetch_token_balance(&svm, &f.route_vault), AMOUNT);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), 0);
}

#[test]
fn finalize_fill_after_unpause_succeeds() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    f.set_paused(&mut svm, true);
    f.finalize(&mut svm).expect_err("paused must reject");

    f.set_paused(&mut svm, false);

    // The same fill (never consumed while paused) finalizes cleanly after unpause.
    f.finalize(&mut svm)
        .expect("finalize after unpause should succeed");
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);
    assert_eq!(
        fetch_fill_receipt(&svm, &f.receipt_pda()).execution_status,
        ExecutionStatus::Finalized
    );
}

// --------------------------------------------------------------------------
// adapter allowlist gate (rejection must preserve state)
// --------------------------------------------------------------------------

#[test]
fn finalize_fill_unregistered_adapter_rejected() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    // Drop the adapter that wrote the evidence from the allowlist.
    let extra = deploy_extra_adapter(&mut svm);
    f.set_adapters(&mut svm, &[extra]);

    assert_eq!(
        f.finalize_err(&mut svm, "evidence from a de-listed adapter must reject"),
        custom_error(SraError::AdapterNotAllowed)
    );
    assert!(svm.get_account(&f.receipt_pda()).is_none());
    assert_eq!(fetch_token_balance(&svm, &f.route_vault), AMOUNT);
}

#[test]
fn finalize_fill_after_readding_adapter_succeeds() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    let extra = deploy_extra_adapter(&mut svm);
    f.set_adapters(&mut svm, &[extra]);
    f.finalize(&mut svm)
        .expect_err("de-listed adapter must reject");

    f.set_adapters(&mut svm, &[ADAPTER_PROGRAM_ID]);
    f.finalize(&mut svm)
        .expect("finalize after re-adding the adapter should succeed");
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);
}

// --------------------------------------------------------------------------
// replay protection
// --------------------------------------------------------------------------

#[test]
fn finalize_fill_replay_rejected() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    f.finalize(&mut svm).expect("first finalize should succeed");
    f.finalize(&mut svm)
        .expect_err("second finalize of the same fill must fail (FillReceipt already exists)");

    // The original receipt is intact and the amount was not moved twice.
    let receipt = fetch_fill_receipt(&svm, &f.receipt_pda());
    assert_eq!(receipt.amount, AMOUNT);
    assert_eq!(receipt.execution_status, ExecutionStatus::Finalized);
    assert_eq!(fetch_token_balance(&svm, &f.execution_vault), AMOUNT);
}

// --------------------------------------------------------------------------
// route-config-hash binding: the signer-seed argument is bound to the route PDA
// --------------------------------------------------------------------------

#[test]
fn finalize_fill_wrong_route_config_hash_rejected() {
    let mut svm = setup_svm();
    let f = setup_finalizable_fill(&mut svm);

    let ix = finalize_fill_ix(
        &f.payer.pubkey(),
        &f.route,
        [0xAB; 32], // tampered route_config_hash
        &f.verified_fill,
        f.fill_id,
        &f.mint,
    );
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &f.payer,
            &[],
            "a route_config_hash that does not derive the route must reject"
        ),
        anchor_error(ErrorCode::ConstraintSeeds)
    );
    assert!(svm.get_account(&f.receipt_pda()).is_none());
    assert_eq!(fetch_token_balance(&svm, &f.route_vault), AMOUNT);
}

// --------------------------------------------------------------------------
// forged / edge VerifiedFill evidence (fabricated, adapter-owned)
// --------------------------------------------------------------------------

struct Fab {
    payer: Keypair,
    mint: Pubkey,
    route: Pubkey,
    route_config_hash: [u8; 32],
}

/// A route with empty vaults — fabricated-evidence tests reject before (or at)
/// the vault check, so funding is unnecessary except where noted.
fn setup_fab(svm: &mut litesvm::LiteSVM) -> Fab {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let (route, route_config_hash) =
        create_sample_route_with_hash(svm, &payer, &owner, mint, [0x31; 32]);
    Fab {
        payer,
        mint,
        route,
        route_config_hash,
    }
}

/// Fabricate `data` as adapter-owned evidence, run finalize with `fill_id_arg`,
/// and return the error. Also asserts no receipt was written.
fn fabricate_and_expect_err(
    svm: &mut litesvm::LiteSVM,
    fix: &Fab,
    owner: Pubkey,
    pda_fill_id: [u8; 32],
    fill_id_arg: [u8; 32],
    data: Vec<u8>,
    label: &str,
) -> TransactionError {
    let verified_fill = fabricate_verified_fill(svm, pda_fill_id, owner, data);
    let ix = finalize_fill_ix(
        &fix.payer.pubkey(),
        &fix.route,
        fix.route_config_hash,
        &verified_fill,
        fill_id_arg,
        &fix.mint,
    );
    let err = send_ix_expect_err(svm, ix, &fix.payer, &[], label);
    assert!(
        svm.get_account(&derive_fill_receipt_pda(&fix.route, &fill_id_arg).0)
            .is_none(),
        "{label}: rejection must not write a FillReceipt"
    );
    err
}

#[test]
fn finalize_fill_wrong_discriminator_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut data = verified_fill_account_bytes(&sample_verified_fill_data(
        fix.route, fix.mint, fill_id, AMOUNT,
    ));
    data[0] ^= 0xff; // corrupt the Anchor discriminator
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            data,
            "wrong discriminator"
        ),
        custom_error(SraError::InvalidFill)
    );
}

#[test]
fn finalize_fill_trailing_bytes_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut data = verified_fill_account_bytes(&sample_verified_fill_data(
        fix.route, fix.mint, fill_id, AMOUNT,
    ));
    data.push(0); // one byte past the exact layout
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            data,
            "trailing bytes"
        ),
        custom_error(SraError::InvalidFill)
    );
}

#[test]
fn finalize_fill_truncated_data_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut data = verified_fill_account_bytes(&sample_verified_fill_data(
        fix.route, fix.mint, fill_id, AMOUNT,
    ));
    data.pop(); // body shorter than the layout
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            data,
            "truncated body"
        ),
        custom_error(SraError::InvalidFill)
    );
}

#[test]
fn finalize_fill_wrong_route_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut vf = sample_verified_fill_data(fix.route, fix.mint, fill_id, AMOUNT);
    vf.route = Pubkey::new_unique(); // evidence for a different route
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "wrong route"
        ),
        custom_error(SraError::InvalidFill)
    );
}

#[test]
fn finalize_fill_wrong_mint_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let other_mint = create_mint(&mut svm, &fix.payer, &fix.payer.pubkey(), 6);
    let fill_id = [0x77; 32];
    let vf = sample_verified_fill_data(fix.route, other_mint, fill_id, AMOUNT);
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "wrong mint"
        ),
        custom_error(SraError::MintMismatch)
    );
}

#[test]
fn finalize_fill_wrong_source_chain_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut vf = sample_verified_fill_data(fix.route, fix.mint, fill_id, AMOUNT);
    vf.source_chain_id = SOURCE_CHAIN_ID + 1;
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "wrong source chain"
        ),
        custom_error(SraError::ChainMismatch)
    );
}

#[test]
fn finalize_fill_fill_id_arg_mismatch_rejected() {
    // The instruction-arg fill_id (which seeds the receipt PDA) must match the
    // fill_id inside the evidence, so the receipt can't be minted under a
    // different id than the fill it records.
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let evidence_fill_id = [0x77; 32];
    let vf = sample_verified_fill_data(fix.route, fix.mint, evidence_fill_id, AMOUNT);
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            evidence_fill_id,
            [0x88; 32], // arg differs from the evidence's fill_id
            verified_fill_account_bytes(&vf),
            "fill_id arg mismatch"
        ),
        custom_error(SraError::InvalidFill)
    );
}

#[test]
fn finalize_fill_amount_below_expected_rejected() {
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let mut vf = sample_verified_fill_data(fix.route, fix.mint, fill_id, AMOUNT);
    vf.amount = 5;
    vf.expected_amount = 10; // delivered less than the source-committed minimum
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "amount below expected"
        ),
        custom_error(SraError::FillAmountTooLow)
    );
}

#[test]
fn finalize_fill_insufficient_route_vault_rejected() {
    // Evidence is fully valid, but the route vault was never funded — the
    // handler refuses to forward more than the vault holds (Across SVM fills are
    // exact, so a short vault is an anomaly, not a partial fill to forward).
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let vf = sample_verified_fill_data(fix.route, fix.mint, fill_id, AMOUNT);
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            ADAPTER_PROGRAM_ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "insufficient route vault"
        ),
        custom_error(SraError::InsufficientVaultBalance)
    );
}

#[test]
fn finalize_fill_non_adapter_owner_rejected() {
    // Valid bytes, but the account is owned by a non-adapter program (here the
    // system program). The owner ∈ allowed_adapters check rejects it before the
    // bytes are ever trusted.
    let mut svm = setup_svm();
    let fix = setup_fab(&mut svm);
    let fill_id = [0x77; 32];
    let vf = sample_verified_fill_data(fix.route, fix.mint, fill_id, AMOUNT);
    assert_eq!(
        fabricate_and_expect_err(
            &mut svm,
            &fix,
            solana_sdk::system_program::ID,
            fill_id,
            fill_id,
            verified_fill_account_bytes(&vf),
            "non-adapter owner"
        ),
        custom_error(SraError::AdapterNotAllowed)
    );
}
