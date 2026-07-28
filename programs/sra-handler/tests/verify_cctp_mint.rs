//! CCTP destination mint verification (`verify_cctp_mint`).
//!
//! The adapter records a `VerifiedFill` only after two independent proofs:
//! Circle's attestation re-verifies over `keccak256(message)` against the
//! live attester set (binding the caller-supplied bytes), and the pinned
//! `["used_nonce", nonce]` receipt proves `receive_message` executed. The
//! bound header/body fields are then cross-checked against the committed
//! route. No mock program backs any of this — verification only reads
//! accounts, so tests plant the transmitter-owned state via `set_account`
//! and sign attestations with a dev secp256k1 key.

mod common;

use anchor_lang::error::ErrorCode;
use common::cctp::*;
use common::*;
use libsecp256k1::SecretKey;
use solana_sdk::{
    clock::Clock, instruction::InstructionError, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::TransactionError,
};
use sra_cctp_adapter::utils::message::{TestBurnMessage, BURN_BODY_LEN, HEADER_LEN};
use sra_cctp_adapter::CctpAdapterError;
use sra_common::types::ManageRouteParams;

const AMOUNT: u64 = 4_000_000;
const MAX_FEE: u64 = 4_000;
/// Matches `sample_create_route_params` — the source chain the route is
/// bound to (Arbitrum, CCTP domain 3).
const SOURCE_CHAIN_ID: u64 = 42_161;
const NONCE: [u8; 32] = [0x77; 32];

fn adapter_error(err: CctpAdapterError) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(err.into()))
}

struct Dest {
    payer: Keypair,
    owner: Keypair,
    mint: Pubkey,
    route: Pubkey,
    attester: SecretKey,
}

/// A funded payer/owner, a 6-decimal mint, a route allowlisting the CCTP
/// adapter, and a planted 1-of-1 transmitter state controlled by `attester`.
fn setup(svm: &mut litesvm::LiteSVM) -> Dest {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let (route, _) = create_cctp_route_with_hash(svm, &payer, &owner, mint, [0x2A; 32]);
    let (attester, attester_addr) = test_attester();
    plant_transmitter_state(svm, 1, vec![attester_addr]);
    Dest {
        payer,
        owner,
        mint,
        route,
        attester,
    }
}

impl Dest {
    /// The canonical verifiable message: used nonce `NONCE`, no executed fee.
    fn canonical(&self) -> TestBurnMessage {
        sample_cctp_message(NONCE, &self.route, &self.mint, AMOUNT, MAX_FEE, 0)
    }

    /// Sign `bytes` with the enabled attester and submit `verify_cctp_mint`
    /// for `NONCE`.
    fn verify_bytes(
        &self,
        svm: &mut litesvm::LiteSVM,
        bytes: Vec<u8>,
    ) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
        let attestation = sign_attestation(&bytes, &[&self.attester]);
        send_ix(
            svm,
            verify_cctp_mint_ix(&self.payer.pubkey(), &self.route, NONCE, bytes, attestation),
            &self.payer,
            &[],
        )
    }

    fn verify(
        &self,
        svm: &mut litesvm::LiteSVM,
        message: &TestBurnMessage,
    ) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
        self.verify_bytes(svm, message.encode())
    }
}

// --------------------------------------------------------------------------
// happy paths
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_happy_path() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    dest.verify(&mut svm, &dest.canonical())
        .expect("verify_cctp_mint should succeed");

    let (verified_fill_pda, expected_bump) = derive_cctp_verified_fill_pda(&NONCE);
    let vf = fetch_cctp_verified_fill(&svm, &verified_fill_pda);
    assert_eq!(vf.adapter, CCTP_ADAPTER_PROGRAM_ID);
    assert_eq!(vf.route, dest.route);
    assert_eq!(vf.fill_id, NONCE);
    assert_eq!(vf.mint, dest.mint);
    // No executed fee: the delivered amount is the full burn amount, and the
    // source-committed minimum survives the worst-case fee.
    assert_eq!(vf.amount, AMOUNT);
    assert_eq!(vf.expected_amount, AMOUNT - MAX_FEE);
    assert_eq!(vf.source_chain_id, SOURCE_CHAIN_ID);
    assert_eq!(vf.bump, expected_bump);
    assert_eq!(vf.created_at, svm.get_sysvar::<Clock>().unix_timestamp);

    // Verification records evidence only — it never moves tokens.
    assert_eq!(
        fetch_token_balance(&svm, &derive_ata(&dest.route, &dest.mint)),
        0
    );
}

#[test]
fn verify_cctp_mint_fee_executed_reduces_delivered_amount() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let mut message = dest.canonical();
    message.fee_executed = 17;
    dest.verify(&mut svm, &message)
        .expect("fee-bearing mint should verify");

    let vf = fetch_cctp_verified_fill(&svm, &derive_cctp_verified_fill_pda(&NONCE).0);
    assert_eq!(vf.amount, AMOUNT - 17, "delivered = amount − fee_executed");
    assert_eq!(vf.expected_amount, AMOUNT - MAX_FEE);
}

#[test]
fn verify_cctp_mint_hook_data_accepted_and_ignored() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let mut message = dest.canonical();
    message.hook_data = vec![0xAB; 64];
    dest.verify(&mut svm, &message)
        .expect("hook_data carries no funds and must not block verification");

    let vf = fetch_cctp_verified_fill(&svm, &derive_cctp_verified_fill_pda(&NONCE).0);
    assert_eq!(vf.amount, AMOUNT);
}

#[test]
fn verify_cctp_mint_emits_cctp_mint_verified() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let bytes = dest.canonical().encode();
    let attestation = sign_attestation(&bytes, &[&dest.attester]);
    let meta = send_ix_with_meta(
        &mut svm,
        verify_cctp_mint_ix(&dest.payer.pubkey(), &dest.route, NONCE, bytes, attestation),
        &dest.payer,
        &[],
    )
    .expect("verify_cctp_mint should succeed");

    let event: sra_cctp_adapter::events::CctpMintVerified =
        extract_anchor_event(&meta.logs).expect("CctpMintVerified emitted");
    assert_eq!(event.verified_fill, derive_cctp_verified_fill_pda(&NONCE).0);
    assert_eq!(event.route, dest.route);
    assert_eq!(event.mint, dest.mint);
    assert_eq!(event.fill_id, NONCE);
    assert_eq!(event.amount, AMOUNT);
    assert_eq!(event.expected_amount, AMOUNT - MAX_FEE);
    assert_eq!(event.source_chain_id, SOURCE_CHAIN_ID);
}

#[test]
fn verify_cctp_mint_two_of_two_threshold_accepted() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);

    // Re-plant with threshold 2; sign with both keys in attester-address order.
    let second = SecretKey::parse(&[0x43; 32]).unwrap();
    let mut keys = vec![&dest.attester, &second];
    keys.sort_by_key(|k| attester_address(k));
    plant_transmitter_state(
        &mut svm,
        2,
        keys.iter().map(|k| attester_address(k)).collect(),
    );
    plant_used_nonce(&mut svm, NONCE, true);

    let bytes = dest.canonical().encode();
    let attestation = sign_attestation(&bytes, &keys);
    send_ix(
        &mut svm,
        verify_cctp_mint_ix(&dest.payer.pubkey(), &dest.route, NONCE, bytes, attestation),
        &dest.payer,
        &[],
    )
    .expect("2-of-2 attestation should verify");
}

// --------------------------------------------------------------------------
// the CCTP-specific forgery: the used-nonce PDA commits to nothing but the
// nonce, so a fabricated message riding a genuinely used nonce must die on
// the attestation — the binding the Across relay-hash re-derivation provides
// structurally is provided here cryptographically.
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_fabricated_message_with_used_nonce_rejected() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    // A genuine attestation exists for the canonical message…
    let attestation = sign_attestation(&dest.canonical().encode(), &[&dest.attester]);

    // …but the caller submits different bytes (attacker-chosen amount) with
    // the same (genuinely used) nonce.
    let mut fabricated = dest.canonical();
    fabricated.amount = AMOUNT * 100;
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            verify_cctp_mint_ix(
                &dest.payer.pubkey(),
                &dest.route,
                NONCE,
                fabricated.encode(),
                attestation,
            ),
            &dest.payer,
            &[],
            "fabricated message must reject",
        ),
        adapter_error(CctpAdapterError::InvalidAttestation)
    );
    assert!(
        svm.get_account(&derive_cctp_verified_fill_pda(&NONCE).0)
            .is_none(),
        "no VerifiedFill on rejection"
    );
}

#[test]
fn verify_cctp_mint_wrong_signer_rejected() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let outsider = SecretKey::parse(&[0x99; 32]).unwrap();
    let bytes = dest.canonical().encode();
    let attestation = sign_attestation(&bytes, &[&outsider]);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            verify_cctp_mint_ix(&dest.payer.pubkey(), &dest.route, NONCE, bytes, attestation),
            &dest.payer,
            &[],
            "non-enabled signer must reject",
        ),
        adapter_error(CctpAdapterError::InvalidAttestation)
    );
}

// --------------------------------------------------------------------------
// used-nonce lifecycle: existence and is_used are the mint-executed proof
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_missing_used_nonce_rejected() {
    // Attested but never received: the nonce PDA does not exist, so the
    // account at the derived address is system-owned and the owner pin fires.
    let mut svm = setup_svm();
    let dest = setup(&mut svm);

    assert_eq!(
        dest.verify(&mut svm, &dest.canonical())
            .expect_err("missing used-nonce receipt must reject")
            .err
            .clone(),
        anchor_error(ErrorCode::ConstraintOwner)
    );
}

#[test]
fn verify_cctp_mint_unused_nonce_rejected() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, false);

    assert_eq!(
        dest.verify(&mut svm, &dest.canonical())
            .expect_err("is_used = false must reject")
            .err
            .clone(),
        adapter_error(CctpAdapterError::NonceNotUsed)
    );
}

// --------------------------------------------------------------------------
// bound-field cross-checks: each mutated field is re-signed (the attestation
// stays valid) so the named check — not the signature — is what rejects.
// --------------------------------------------------------------------------

fn assert_check_fires(name: &str, mutate: fn(&mut TestBurnMessage), expected: CctpAdapterError) {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let mut message = dest.canonical();
    mutate(&mut message);
    assert_eq!(
        dest.verify(&mut svm, &message)
            .expect_err("mutated field must reject")
            .err
            .clone(),
        adapter_error(expected),
        "{name}"
    );
    assert!(
        svm.get_account(&derive_cctp_verified_fill_pda(&NONCE).0)
            .is_none(),
        "{name}: no VerifiedFill on rejection"
    );
}

#[test]
fn verify_cctp_mint_every_bound_field_check_fires() {
    assert_check_fires(
        "header version",
        |m| m.version += 1,
        CctpAdapterError::MalformedMessage,
    );
    assert_check_fires(
        "destination domain",
        |m| m.destination_domain = 0,
        CctpAdapterError::DestinationDomainMismatch,
    );
    assert_check_fires(
        "nonce (message ≠ argument)",
        |m| m.nonce = [0x88; 32],
        CctpAdapterError::NonceMismatch,
    );
    assert_check_fires(
        "recipient program",
        |m| m.recipient = Pubkey::new_from_array([0x99; 32]),
        CctpAdapterError::InvalidRecipientProgram,
    );
    assert_check_fires(
        "body version",
        |m| m.body_version = 0,
        CctpAdapterError::MalformedMessage,
    );
    assert_check_fires(
        "mint recipient",
        |m| m.mint_recipient = Pubkey::new_from_array([0x99; 32]),
        CctpAdapterError::MintRecipientMismatch,
    );
    assert_check_fires(
        "source domain (unmapped)",
        |m| m.source_domain = 99,
        CctpAdapterError::SourceDomainMismatch,
    );
    assert_check_fires(
        "source domain (mapped, wrong chain)",
        |m| m.source_domain = 0, // Ethereum — not the route's Arbitrum
        CctpAdapterError::SourceDomainMismatch,
    );
    assert_check_fires(
        "fee_executed above max_fee",
        |m| m.fee_executed = m.max_fee + 1,
        CctpAdapterError::FeeExceedsMaxFee,
    );
    assert_check_fires(
        "max_fee not below amount",
        |m| m.max_fee = m.amount,
        CctpAdapterError::FeeExceedsMaxFee,
    );
}

#[test]
fn verify_cctp_mint_amount_bound_by_attestation() {
    // The complement of the re-signed battery: mutate WITHOUT re-signing and
    // the signature itself is what rejects — amount is attestation-bound.
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let attestation = sign_attestation(&dest.canonical().encode(), &[&dest.attester]);
    let mut tampered = dest.canonical();
    tampered.amount += 1;
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            verify_cctp_mint_ix(
                &dest.payer.pubkey(),
                &dest.route,
                NONCE,
                tampered.encode(),
                attestation,
            ),
            &dest.payer,
            &[],
            "stale attestation over mutated amount must reject",
        ),
        adapter_error(CctpAdapterError::InvalidAttestation)
    );
}

// --------------------------------------------------------------------------
// malformed bytes: attested (re-signed) but structurally invalid
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_malformed_bytes_rejected() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let full = dest.canonical().encode();

    // Header below its floor.
    assert_eq!(
        dest.verify_bytes(&mut svm, full[..HEADER_LEN - 1].to_vec())
            .expect_err("truncated header must reject")
            .err
            .clone(),
        adapter_error(CctpAdapterError::MalformedMessage)
    );
    // Body below its floor.
    assert_eq!(
        dest.verify_bytes(&mut svm, full[..HEADER_LEN + BURN_BODY_LEN - 1].to_vec())
            .expect_err("truncated body must reject")
            .err
            .clone(),
        adapter_error(CctpAdapterError::MalformedMessage)
    );
    // A non-zero byte in the amount slot's top-24 padding.
    let mut padded = full.clone();
    padded[HEADER_LEN + 68] = 0x01;
    assert_eq!(
        dest.verify_bytes(&mut svm, padded)
            .expect_err("non-zero slot padding must reject")
            .err
            .clone(),
        adapter_error(CctpAdapterError::MalformedMessage)
    );
}

// --------------------------------------------------------------------------
// transmitter-state pinning and validity
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_wrong_transmitter_account_rejected() {
    // Pointing verification at any account other than the transmitter PDA
    // fails the seeds constraint.
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let bytes = dest.canonical().encode();
    let attestation = sign_attestation(&bytes, &[&dest.attester]);
    let mut ix = verify_cctp_mint_ix(&dest.payer.pubkey(), &dest.route, NONCE, bytes, attestation);
    let slot = ix
        .accounts
        .iter()
        .position(|m| m.pubkey == derive_transmitter_state_pda())
        .expect("transmitter meta present");
    ix.accounts[slot].pubkey = Pubkey::new_unique();

    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "non-PDA transmitter account must reject",
        ),
        anchor_error(ErrorCode::ConstraintSeeds)
    );
}

#[test]
fn verify_cctp_mint_transmitter_not_transmitter_owned_rejected() {
    // A look-alike state at the correct PDA but owned by another program is a
    // forged trust root — the owner constraint rejects it.
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    let mut account = svm
        .get_account(&derive_transmitter_state_pda())
        .expect("planted transmitter state");
    account.owner = PROGRAM_ID; // handler program, NOT the transmitter
    svm.set_account(derive_transmitter_state_pda(), account)
        .unwrap();

    assert_eq!(
        dest.verify(&mut svm, &dest.canonical())
            .expect_err("non-transmitter-owned state must reject")
            .err
            .clone(),
        anchor_error(ErrorCode::ConstraintOwner)
    );
}

#[test]
fn verify_cctp_mint_zero_threshold_state_rejected() {
    // threshold = 0 would make an empty attestation "verify" — the state
    // parse fails closed before any signature checking.
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_transmitter_state(&mut svm, 0, vec![attester_address(&dest.attester)]);
    plant_used_nonce(&mut svm, NONCE, true);

    let bytes = dest.canonical().encode();
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            verify_cctp_mint_ix(&dest.payer.pubkey(), &dest.route, NONCE, bytes, vec![]),
            &dest.payer,
            &[],
            "zero-threshold state must reject",
        ),
        adapter_error(CctpAdapterError::InvalidTransmitterState)
    );
}

// --------------------------------------------------------------------------
// replay: VerifiedFill is `init`, so a second verification of the same nonce
// fails while the first artifact still exists.
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_duplicate_nonce_rejected() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    dest.verify(&mut svm, &dest.canonical())
        .expect("first verify should succeed");
    dest.verify(&mut svm, &dest.canonical())
        .expect_err("second verify of the same nonce must fail (VerifiedFill already exists)");

    // The original artifact is intact.
    let vf = fetch_cctp_verified_fill(&svm, &derive_cctp_verified_fill_pda(&NONCE).0);
    assert_eq!(vf.fill_id, NONCE);
}

// --------------------------------------------------------------------------
// paused route: pause gates fund movement (finalize), not evidence-recording.
// --------------------------------------------------------------------------

#[test]
fn verify_cctp_mint_paused_route_allowed() {
    let mut svm = setup_svm();
    let dest = setup(&mut svm);
    plant_used_nonce(&mut svm, NONCE, true);

    send_ix(
        &mut svm,
        manage_route_ix(
            &dest.owner.pubkey(),
            &dest.route,
            ManageRouteParams {
                pause: Some(true),
                adapters: None,
                executors: None,
            },
            &[],
        ),
        &dest.payer,
        &[&dest.owner],
    )
    .expect("pause should succeed");
    assert!(fetch_route(&svm, &dest.route).paused);

    dest.verify(&mut svm, &dest.canonical())
        .expect("verify on a paused route should succeed");
    let vf = fetch_cctp_verified_fill(&svm, &derive_cctp_verified_fill_pda(&NONCE).0);
    assert_eq!(vf.route, dest.route);
    assert_eq!(vf.amount, AMOUNT);
}
