//! Destination multi-fill / multi-route isolation — properties that only emerge
//! once more than one fill or route coexists, which the per-instruction and
//! single-fill lifecycle suites never exercise.
//!
//! Two complementary invariants:
//! - **Same route, many fills**: the `FillReceipt` is per `(route, fill_id)`, but
//!   the execution vault is per-route and SHARED by every fill of that route.
//!   Finalizing several fills co-mingles them in one vault while writing
//!   independent receipts; recovering one drains the whole vault and advances
//!   only that receipt — fund-safe (no double payout), the others stay valid.
//! - **Different routes, same owner**: vaults and receipts are keyed on the route
//!   PDA, so two routes under one owner are fully isolated — operating on one
//!   never touches the other.
//!
//! Both drive the genuine path (relayer fill -> verify_across_fill ->
//! finalize_fill) and use `recover_funds` as the terminal step (no action_config
//! infra needed — the invariants are about vault/receipt keying, not actions).

mod common;

use common::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_common::{errors::SraError, types::ExecutionStatus};

const AMOUNT_A: u64 = 3_000_000;
const AMOUNT_B: u64 = 5_000_000;
/// Matches `sample_create_route_params` — the route's bound source chain.
const SOURCE_CHAIN_ID: u64 = 42_161;
/// Matches `sample_relay_data`'s fill deadline (the relay hash covers it).
const FILL_DEADLINE: u32 = 1_900_000_000;

/// Drive one fill (distinct `deposit_id` -> distinct fill_id / relay_hash) all the
/// way to `Finalized` on `route`: relayer fills the route vault, the adapter
/// records the VerifiedFill, the handler finalizes it. Returns `(fill_id, receipt)`.
fn finalize_one(
    svm: &mut litesvm::LiteSVM,
    payer: &Keypair,
    route: &Pubkey,
    route_config_hash: [u8; 32],
    mint: Pubkey,
    deposit_id: [u8; 32],
    amount: u64,
) -> ([u8; 32], Pubkey) {
    let mut relay_data = sample_relay_data(*route, mint, SOURCE_CHAIN_ID, amount);
    relay_data.deposit_id = deposit_id;
    let relay_hash = relay_hash_for(&relay_data);
    let route_vault = derive_ata(route, &mint);

    let relayer = funded_keypair(svm, 100 * SOL);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(svm, payer, payer, &mint, &relayer_ata, amount);
    send_ix(
        svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &route_vault,
            relay_hash,
            amount,
            FILL_DEADLINE,
        ),
        &relayer,
        &[],
    )
    .expect("relayer fill should succeed");
    send_ix(
        svm,
        verify_across_fill_ix(&payer.pubkey(), route, relay_hash, relay_data),
        payer,
        &[],
    )
    .expect("verify_across_fill should succeed");
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    send_ix(
        svm,
        finalize_fill_ix(
            &payer.pubkey(),
            route,
            route_config_hash,
            &verified_fill,
            relay_hash,
            &mint,
        ),
        payer,
        &[],
    )
    .expect("finalize_fill should succeed");
    let (receipt, _) = derive_fill_receipt_pda(route, &relay_hash);
    (relay_hash, receipt)
}

#[test]
fn multiple_fills_same_route_share_execution_vault_independent_receipts() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let (route, hash) = create_sample_route_with_hash(&mut svm, &payer, &owner, mint, [0x39; 32]);

    let route_vault = derive_ata(&route, &mint);
    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);

    // Two distinct fills on the SAME route (distinct deposit_id -> distinct fill_id).
    let (fill_a, receipt_a) =
        finalize_one(&mut svm, &payer, &route, hash, mint, [0xA1; 32], AMOUNT_A);
    // The first finalize drains the route vault before the second fill arrives.
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);
    let (fill_b, receipt_b) =
        finalize_one(&mut svm, &payer, &route, hash, mint, [0xB2; 32], AMOUNT_B);
    assert_ne!(
        fill_a, fill_b,
        "distinct deposit_id must yield a distinct fill_id"
    );
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);

    // Two independent receipts, each Finalized with its OWN amount...
    let ra = fetch_fill_receipt(&svm, &receipt_a);
    let rb = fetch_fill_receipt(&svm, &receipt_b);
    assert_eq!(ra.fill_id, fill_a);
    assert_eq!(rb.fill_id, fill_b);
    assert_eq!(ra.amount, AMOUNT_A);
    assert_eq!(rb.amount, AMOUNT_B);
    assert_eq!(ra.execution_status, ExecutionStatus::Finalized);
    assert_eq!(rb.execution_status, ExecutionStatus::Finalized);
    // ...but ONE shared execution vault holding the co-mingled total (the vault is
    // keyed on the route PDA alone — per-route, not per-fill).
    assert_eq!(
        fetch_token_balance(&svm, &execution_vault),
        AMOUNT_A + AMOUNT_B
    );

    // Recovering fill A drains the WHOLE shared vault to the owner and advances
    // only A's receipt; B's receipt is untouched (independent lifecycle).
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &route, &fill_a, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover fill A should succeed");
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT_A + AMOUNT_B);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_a).execution_status,
        ExecutionStatus::Recovered
    );
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_b).execution_status,
        ExecutionStatus::Finalized
    );

    // Single-drain conservation: A's recovery already swept the shared vault, so
    // recovering B now moves nothing (NothingToRecover) and B stays Finalized. The
    // payout is capped at the vault balance, not the sum of receipt amounts — a
    // receipt is a lifecycle marker, not a per-fill fund claim. Recovery is
    // owner-gated and owner-ATA-only, so this is loose-but-safe, never a double-spend.
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            recover_funds_ix(&owner.pubkey(), &route, &fill_b, &mint, &owner_ata),
            &owner,
            &[],
            "recover against an emptied shared vault must not double-pay",
        ),
        custom_error(SraError::NothingToRecover)
    );
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT_A + AMOUNT_B);
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_b).execution_status,
        ExecutionStatus::Finalized
    );
}

#[test]
fn two_routes_same_owner_fully_isolated() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL); // one owner, two routes
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);

    // Two coexisting routes: same owner, same adapter, distinct salt + action_config
    // -> distinct route PDA.
    let (route_a, hash_a) =
        create_sample_route_with_hash(&mut svm, &payer, &owner, mint, [0xA0; 32]);
    let (route_b, hash_b) =
        create_sample_route_with_hash(&mut svm, &payer, &owner, mint, [0xB0; 32]);
    assert_ne!(route_a, route_b);

    let exec_vault_a = derive_ata(&derive_execution_authority_pda(&route_a).0, &mint);
    let exec_vault_b = derive_ata(&derive_execution_authority_pda(&route_b).0, &mint);
    // Vaults are keyed on the route PDA, so distinct routes have distinct vaults.
    assert_ne!(exec_vault_a, exec_vault_b);

    let (fill_a, receipt_a) = finalize_one(
        &mut svm, &payer, &route_a, hash_a, mint, [0xAA; 32], AMOUNT_A,
    );
    let (_fill_b, receipt_b) = finalize_one(
        &mut svm, &payer, &route_b, hash_b, mint, [0xBB; 32], AMOUNT_B,
    );

    // Per-route SEPARATE vaults (keyed on the route PDA) — NOT shared, unlike
    // multiple fills of a single route.
    assert_eq!(fetch_token_balance(&svm, &exec_vault_a), AMOUNT_A);
    assert_eq!(fetch_token_balance(&svm, &exec_vault_b), AMOUNT_B);

    // Recovering route A touches only route A; route B is completely unaffected.
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &route_a, &fill_a, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover route A should succeed");
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT_A); // only A's funds
    assert_eq!(fetch_token_balance(&svm, &exec_vault_a), 0);
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_a).execution_status,
        ExecutionStatus::Recovered
    );
    // Route B: vault and receipt untouched by route A's recovery.
    assert_eq!(fetch_token_balance(&svm, &exec_vault_b), AMOUNT_B);
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_b).execution_status,
        ExecutionStatus::Finalized
    );
}
