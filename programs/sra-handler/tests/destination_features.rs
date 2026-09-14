//! Destination multi-fill / multi-route isolation — properties that only emerge
//! once more than one fill or route coexists, which the per-instruction and
//! single-fill lifecycle suites never exercise.
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
fn recovery_releases_route_for_next_fill() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let (route, hash) = create_sample_route_with_hash(&mut svm, &payer, &owner, mint, [0x39; 32]);

    let route_vault = derive_ata(&route, &mint);
    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);

    let (fill_a, receipt_a) =
        finalize_one(&mut svm, &payer, &route, hash, mint, [0xA1; 32], AMOUNT_A);
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);
    assert_eq!(fetch_route(&svm, &route).active_fill, Some(fill_a));
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &route, &fill_a, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover A releases the route");
    assert_eq!(fetch_route(&svm, &route).active_fill, None);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT_A);
    let (fill_b, receipt_b) =
        finalize_one(&mut svm, &payer, &route, hash, mint, [0xB2; 32], AMOUNT_B);
    assert_ne!(
        fill_a, fill_b,
        "distinct deposit_id must yield a distinct fill_id"
    );
    assert_eq!(fetch_token_balance(&svm, &route_vault), 0);

    let ra = fetch_fill_receipt(&svm, &receipt_a);
    let rb = fetch_fill_receipt(&svm, &receipt_b);
    assert_eq!(ra.fill_id, fill_a);
    assert_eq!(rb.fill_id, fill_b);
    assert_eq!(ra.amount, AMOUNT_A);
    assert_eq!(rb.amount, AMOUNT_B);
    assert_eq!(ra.execution_status, ExecutionStatus::Recovered);
    assert_eq!(rb.execution_status, ExecutionStatus::Finalized);
    assert_eq!(fetch_route(&svm, &route).active_fill, Some(fill_b));
    assert_eq!(fetch_token_balance(&svm, &execution_vault), AMOUNT_B);
    let addresses = [route, execution_vault, owner_ata, receipt_a, receipt_b];
    let before = addresses.map(|key| svm.get_account(&key));
    assert_eq!(
        send_ix_expect_err(
            &mut svm,
            recover_funds_ix(&owner.pubkey(), &route, &fill_a, &mint, &owner_ata),
            &owner,
            &[],
            "recovered A cannot drain active B",
        ),
        custom_error(SraError::InvalidActiveFill)
    );
    assert_eq!(addresses.map(|key| svm.get_account(&key)), before);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &route, &fill_b, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover B succeeds");
    assert_eq!(fetch_token_balance(&svm, &owner_ata), AMOUNT_A + AMOUNT_B);
    assert_eq!(fetch_token_balance(&svm, &execution_vault), 0);
    assert_eq!(fetch_route(&svm, &route).active_fill, None);
    assert_eq!(
        fetch_fill_receipt(&svm, &receipt_b).execution_status,
        ExecutionStatus::Recovered
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
