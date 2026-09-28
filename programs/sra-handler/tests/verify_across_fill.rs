//! Across destination fill verification (`verify_across_fill`).
//!
//! The adapter records a `VerifiedFill` only after it has independently
//! confirmed an Across fill: the `fill_status` account is pinned to
//! `["fills", relay_hash]` under the Across program and owner-checked, its
//! status must be `Filled`, and the caller-supplied `relay_data` must re-derive
//! the same `relay_hash` (the cryptographic binding). The route-bound fields are
//! then cross-checked against the committed route. There is no CPI into the
//! SpokePool to backstop any of this — every check is the adapter's own.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use mock_spoke_pool::FillStatus;
use solana_clock::Clock;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use sra_across_adapter::utils::svm_spoke::types::RelayData;
use sra_across_adapter::AcrossAdapterError;
use sra_common::types::ManageRouteParams;

const OUTPUT_AMOUNT: u64 = 4_000_000;
/// Matches `sample_create_route_params` — the source chain the sample route is
/// bound to.
const SOURCE_CHAIN_ID: u64 = 42_161;
const FILL_DEADLINE: u32 = 1_900_000_000;

struct Dest {
    payer: Keypair,
    owner: Keypair,
    mint: Pubkey,
    route: Pubkey,
}

/// A funded payer/owner, a 6-decimal mint, and the sample route bound to that
/// mint and `SOURCE_CHAIN_ID`.
fn setup_route(svm: &mut litesvm::LiteSVM) -> Dest {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, SOL);
    let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
    let route = create_sample_route(svm, &payer, &owner, mint, [0x29; 32]);
    Dest {
        payer,
        owner,
        mint,
        route,
    }
}

/// Plant a `FillStatusAccount` with an arbitrary status (no token movement) so
/// the verify-side checks can be exercised against an Across-owned account.
fn plant_status(svm: &mut litesvm::LiteSVM, relay_hash: [u8; 32], status: FillStatus) {
    let relayer = funded_keypair(svm, SOL);
    send_ix(
        svm,
        mock_fill_ix(&relayer.pubkey(), relay_hash, status),
        &relayer,
        &[],
    )
    .expect("mock_fill should succeed");
}

fn deliver_to_route_vault(
    svm: &mut litesvm::LiteSVM,
    dest: &Dest,
    route_vault: Pubkey,
    relay_hash: [u8; 32],
    amount: u64,
) {
    let relayer = funded_keypair(svm, SOL);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &dest.mint);
    mint_to(
        svm,
        &dest.payer,
        &dest.payer,
        &dest.mint,
        &relayer_ata,
        amount,
    );
    send_ix(
        svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &dest.mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            amount,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .expect("fill should succeed");
}

// --------------------------------------------------------------------------
// happy path
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_happy_path() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);

    // A realistic relayer fill: move the output amount into the route's vault and
    // write a `Filled` status, exactly as the deployed Across program does.
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let route_vault = derive_ata(&dest.route, &dest.mint);

    let relayer = funded_keypair(&mut svm, 100 * SOL);
    let relayer_ata = create_ata(&mut svm, &relayer, &relayer.pubkey(), &dest.mint);
    // The mint authority is the payer that created the mint.
    mint_to(
        &mut svm,
        &dest.payer,
        &dest.payer,
        &dest.mint,
        &relayer_ata,
        OUTPUT_AMOUNT,
    );
    send_ix(
        &mut svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &dest.mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            OUTPUT_AMOUNT,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .expect("fill_relay should succeed");
    assert_eq!(fetch_token_balance(&svm, &route_vault), OUTPUT_AMOUNT);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    send_ix(&mut svm, ix, &dest.payer, &[]).expect("verify_across_fill should succeed");

    let (verified_fill_pda, expected_bump) = derive_verified_fill_pda(&relay_hash);
    let vf = fetch_verified_fill(&svm, &verified_fill_pda);
    assert_eq!(vf.adapter, ADAPTER_PROGRAM_ID);
    assert_eq!(vf.route, dest.route);
    assert_eq!(vf.fill_id, relay_hash);
    assert_eq!(vf.mint, dest.mint);
    assert_eq!(vf.amount, OUTPUT_AMOUNT);
    assert_eq!(vf.expected_amount, OUTPUT_AMOUNT);
    assert_eq!(vf.source_chain_id, SOURCE_CHAIN_ID);
    assert_eq!(vf.bump, expected_bump);
    assert_eq!(vf.created_at, svm.get_sysvar::<Clock>().unix_timestamp);

    // Verification records evidence only — it never moves the filled tokens.
    assert_eq!(fetch_token_balance(&svm, &route_vault), OUTPUT_AMOUNT);
}

#[test]
fn verify_across_fill_emits_across_fill_verified() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    let meta = send_ix_with_meta(&mut svm, ix, &dest.payer, &[])
        .expect("verify_across_fill should succeed");

    let event: sra_across_adapter::events::AcrossFillVerified =
        extract_anchor_event(&meta.logs).expect("AcrossFillVerified emitted");
    assert_eq!(event.route, dest.route);
    assert_eq!(event.fill_id, relay_hash);
    assert_eq!(event.mint, dest.mint);
    assert_eq!(event.amount, OUTPUT_AMOUNT);
    assert_eq!(event.expected_amount, OUTPUT_AMOUNT);
    assert_eq!(event.source_chain_id, SOURCE_CHAIN_ID);
}

// --------------------------------------------------------------------------
// fill status must be Filled
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_unfilled_rejected() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Unfilled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(&mut svm, ix, &dest.payer, &[], "unfilled must reject"),
        adapter_error(AcrossAdapterError::FillNotCompleted)
    );
    assert!(
        svm.get_account(&derive_verified_fill_pda(&relay_hash).0)
            .is_none(),
        "no VerifiedFill on rejection"
    );
}

#[test]
fn verify_across_fill_requested_slow_fill_rejected() {
    // A slow-fill *request* is not a completed fill — the tokens have not been
    // delivered, so verification must not record evidence.
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::RequestedSlowFill);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "slow-fill request must reject"
        ),
        adapter_error(AcrossAdapterError::FillNotCompleted)
    );
}

#[test]
fn slow_fill_cannot_consume_unrelated_route_liquidity() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let route_vault = derive_ata(&dest.route, &dest.mint);

    let mut unrelated_relay_data =
        sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, 3_000_000);
    unrelated_relay_data.deposit_id = [0x66; 32];
    deliver_to_route_vault(
        &mut svm,
        &dest,
        route_vault,
        relay_hash_for(&unrelated_relay_data),
        3_000_000,
    );
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    deliver_to_route_vault(&mut svm, &dest, route_vault, relay_hash, 1_000_000);
    assert_eq!(fetch_token_balance(&svm, &route_vault), OUTPUT_AMOUNT);

    set_time(&mut svm, i64::from(relay_data.exclusivity_deadline) + 1);
    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "ambiguous fill amount must reject",
        ),
        adapter_error(AcrossAdapterError::ExecutedAmountUnavailable)
    );

    assert!(svm
        .get_account(&derive_verified_fill_pda(&relay_hash).0)
        .is_none());
    assert_eq!(fetch_token_balance(&svm, &route_vault), OUTPUT_AMOUNT);
}

#[test]
fn verify_across_fill_at_exclusivity_deadline_allowed() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);
    set_time(&mut svm, i64::from(relay_data.exclusivity_deadline));

    send_ix(
        &mut svm,
        verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data),
        &dest.payer,
        &[],
    )
    .expect("verification at exclusivity deadline should succeed");
}

#[test]
fn verify_across_fill_full_lifetime_exclusivity_allows_late_verification() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let mut relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    relay_data.exclusivity_deadline = relay_data.fill_deadline;
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);
    set_time(&mut svm, i64::from(relay_data.fill_deadline) + 1);

    send_ix(
        &mut svm,
        verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data),
        &dest.payer,
        &[],
    )
    .expect("full-lifetime exclusivity should allow late verification");

    let verified = fetch_verified_fill(&svm, &derive_verified_fill_pda(&relay_hash).0);
    assert_eq!(verified.amount, OUTPUT_AMOUNT);
    assert_eq!(verified.expected_amount, OUTPUT_AMOUNT);
}

// --------------------------------------------------------------------------
// relay-hash binding: every relay_data field is folded into the hash, so a
// stale relay_hash with any mutated field fails the re-derivation.
// --------------------------------------------------------------------------

fn assert_field_bound_to_relay_hash(name: &str, mutate: fn(&mut RelayData)) {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);

    let canonical = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&canonical);
    // A genuine Filled fill exists at the canonical relay hash...
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    // ...but the caller supplies relay_data with one field changed while pointing
    // at the canonical relay hash. The re-derivation no longer matches.
    let mut tampered = canonical.clone();
    mutate(&mut tampered);
    assert_ne!(
        relay_hash_for(&tampered),
        relay_hash,
        "{name}: mutation must change the relay hash (test premise)"
    );

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, tampered);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "tampered relay_data must reject"
        ),
        adapter_error(AcrossAdapterError::RelayHashMismatch),
        "{name}: must fail relay-hash re-derivation"
    );
    assert!(
        svm.get_account(&derive_verified_fill_pda(&relay_hash).0)
            .is_none(),
        "{name}: no VerifiedFill on rejection"
    );
}

#[test]
fn verify_across_fill_every_relay_data_field_is_bound() {
    assert_field_bound_to_relay_hash("depositor", |rd| {
        rd.depositor = Pubkey::new_from_array([0x99; 32])
    });
    assert_field_bound_to_relay_hash("recipient", |rd| {
        rd.recipient = Pubkey::new_from_array([0x99; 32])
    });
    assert_field_bound_to_relay_hash("exclusive_relayer", |rd| {
        rd.exclusive_relayer = Pubkey::new_from_array([0x99; 32])
    });
    assert_field_bound_to_relay_hash("input_token", |rd| {
        rd.input_token = Pubkey::new_from_array([0x99; 32])
    });
    assert_field_bound_to_relay_hash("output_token", |rd| {
        rd.output_token = Pubkey::new_from_array([0x99; 32])
    });
    assert_field_bound_to_relay_hash("input_amount", |rd| rd.input_amount = [0x99; 32]);
    assert_field_bound_to_relay_hash("output_amount", |rd| rd.output_amount += 1);
    assert_field_bound_to_relay_hash("origin_chain_id", |rd| rd.origin_chain_id += 1);
    assert_field_bound_to_relay_hash("deposit_id", |rd| rd.deposit_id = [0x99; 32]);
    assert_field_bound_to_relay_hash("fill_deadline", |rd| rd.fill_deadline += 1);
    assert_field_bound_to_relay_hash("exclusivity_deadline", |rd| rd.exclusivity_deadline += 1);
    assert_field_bound_to_relay_hash("message", |rd| rd.message = vec![0x00]);
}

#[test]
fn verify_across_fill_wrong_fill_status_pda_rejected() {
    // A fill_status account that is not `["fills", relay_hash]` fails the seeds
    // constraint — the caller cannot point verification at an unrelated fill.
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    // Build the instruction by hand with a fill_status seeded by a *different*
    // relay hash than the one passed as the argument.
    let other_fill_status = derive_fill_status_pda(&[0xEE; 32]).0;
    let mut ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    let slot = ix
        .accounts
        .iter()
        .position(|m| m.pubkey == derive_fill_status_pda(&relay_hash).0)
        .expect("fill_status meta present");
    ix.accounts[slot].pubkey = other_fill_status;

    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "wrong fill_status pda must reject"
        ),
        anchor_error(ErrorCode::ConstraintSeeds)
    );
}

// --------------------------------------------------------------------------
// route binding: relay_data is self-consistent (matches relay_hash) but its
// route-bound fields disagree with the committed route.
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_recipient_not_route_rejected() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    // recipient points somewhere other than the route PDA.
    let relay_data = sample_relay_data(
        Pubkey::new_unique(),
        dest.mint,
        SOURCE_CHAIN_ID,
        OUTPUT_AMOUNT,
    );
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "recipient mismatch must reject"
        ),
        adapter_error(AcrossAdapterError::RecipientMismatch)
    );
}

#[test]
fn verify_across_fill_output_token_not_expected_mint_rejected() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let other_mint = create_mint(&mut svm, &dest.payer, &dest.payer.pubkey(), 6);
    let relay_data = sample_relay_data(dest.route, other_mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "output token mismatch must reject"
        ),
        adapter_error(AcrossAdapterError::OutputTokenMismatch)
    );
}

#[test]
fn verify_across_fill_origin_chain_not_route_source_rejected() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID + 1, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "origin chain mismatch must reject"
        ),
        adapter_error(AcrossAdapterError::OriginChainMismatch)
    );
}

// --------------------------------------------------------------------------
// fill_status ownership / lifecycle
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_fill_status_not_across_owned_rejected() {
    // An account at the correct fill_status PDA but owned by another program is
    // a forged fill — the owner constraint rejects it.
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);

    // Plant a look-alike `Filled` FillStatusAccount owned by the handler program
    // (not the Across program) at the canonical fill_status address.
    let (fill_status_pda, _) = derive_fill_status_pda(&relay_hash);
    let mut data =
        <mock_spoke_pool::FillStatusAccount as anchor_lang::Discriminator>::DISCRIMINATOR.to_vec();
    anchor_lang::AnchorSerialize::serialize(
        &mock_spoke_pool::FillStatusAccount {
            status: FillStatus::Filled,
            relayer: Pubkey::new_unique(),
            fill_deadline: FILL_DEADLINE,
        },
        &mut data,
    )
    .unwrap();
    svm.set_account(
        fill_status_pda,
        solana_account::Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len()),
            data,
            owner: PROGRAM_ID, // handler program, NOT the Across program
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "non-Across-owned fill must reject"
        ),
        anchor_error(ErrorCode::ConstraintOwner)
    );
}

#[test]
fn verify_across_fill_fill_status_closed_rejected() {
    // The relayer-closure race: a fill_status reclaimed before verification is no
    // longer Across-owned, so the owner constraint rejects it cleanly (no panic).
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);

    let relayer = funded_keypair(&mut svm, SOL);
    send_ix(
        &mut svm,
        mock_fill_ix(&relayer.pubkey(), relay_hash, FillStatus::Filled),
        &relayer,
        &[],
    )
    .expect("mock_fill should succeed");
    let (fill_status_pda, _) = derive_fill_status_pda(&relay_hash);
    let timestamp = now(&svm).max(1);
    set_time(&mut svm, timestamp);
    send_ix(
        &mut svm,
        close_fill_pda_ix(&relayer.pubkey(), &fill_status_pda),
        &relayer,
        &[],
    )
    .expect("close_fill_pda should succeed");

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            ix,
            &dest.payer,
            &[],
            "closed fill_status must reject"
        ),
        anchor_error(ErrorCode::ConstraintOwner)
    );
}

// --------------------------------------------------------------------------
// replay: VerifiedFill is `init`, so a second verification of the same fill
// fails while the first artifact still exists.
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_duplicate_relay_hash_rejected() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);
    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    send_ix(
        &mut svm,
        verify_across_fill_ix(
            &dest.payer.pubkey(),
            &dest.route,
            relay_hash,
            relay_data.clone(),
        ),
        &dest.payer,
        &[],
    )
    .expect("first verify should succeed");

    send_ix(
        &mut svm,
        verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data),
        &dest.payer,
        &[],
    )
    .expect_err("second verify of the same fill must fail (VerifiedFill already exists)");

    // The original artifact is intact.
    let vf = fetch_verified_fill(&svm, &derive_verified_fill_pda(&relay_hash).0);
    assert_eq!(vf.fill_id, relay_hash);
}

// --------------------------------------------------------------------------
// paused route: pause gates fund movement (finalize), not evidence-recording.
// --------------------------------------------------------------------------

#[test]
fn verify_across_fill_paused_route_allowed() {
    let mut svm = setup_svm();
    let dest = setup_route(&mut svm);

    // Pause the route. Recording the fill must still succeed — pausing only blocks
    // the later fund-moving step, and a fill that arrives while paused must remain
    // recoverable once the owner unpauses.
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

    let relay_data = sample_relay_data(dest.route, dest.mint, SOURCE_CHAIN_ID, OUTPUT_AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    plant_status(&mut svm, relay_hash, FillStatus::Filled);

    let ix = verify_across_fill_ix(&dest.payer.pubkey(), &dest.route, relay_hash, relay_data);
    send_ix(&mut svm, ix, &dest.payer, &[]).expect("verify on a paused route should succeed");

    let vf = fetch_verified_fill(&svm, &derive_verified_fill_pda(&relay_hash).0);
    assert_eq!(vf.route, dest.route);
    assert_eq!(vf.amount, OUTPUT_AMOUNT);
}
