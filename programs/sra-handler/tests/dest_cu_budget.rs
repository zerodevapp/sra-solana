//! Destination compute-unit regression assertions.
//!
//! The production destination fill is a versioned transaction over an ALT that
//! batches the two-instruction flow `[verify_across_fill (adapter, IX 1),
//! finalize_fill (handler, IX 2)]`, requesting a 400k CU limit — the same budget
//! the source deposit requests. The owner's route-committed actions then run in a
//! follow-up `execute_actions` transaction (the fill receipt gates the sequence,
//! so they need not share a transaction).
//!
//! These tests drive the genuine flow through real instructions (relayer fill via
//! the mock SpokePool → adapter `verify_across_fill` → handler `finalize_fill` →
//! `execute_actions`), read the actual consumption from LiteSVM's
//! `TransactionMetadata`, log it with headroom, and assert it stays under the
//! production budget — so an Anchor/dependency upgrade (or our own code) that
//! silently inflates the CU cost fails here instead of on mainnet. The structure
//! mirrors `source_cu_budget.rs`.
//!
//! Coverage:
//! - the 2-IX destination flow (`verify_across_fill` + `finalize_fill`), the
//!   production fill transaction;
//! - `finalize_fill` + `execute_actions` combined at the maximum SplTransfer
//!   fan-out (four recipients);
//! - `finalize_fill` + `execute_actions` combined for the heaviest action — a
//!   verified-CPI swap (scratch move + inner CPI + balance-diff post-check +
//!   sweep), co-signed by the route-committed quoter.
//!
//! Measurement notes (identical methodology to `source_cu_budget.rs`):
//! - The runtime CU limit is set to the Solana maximum so a flow that exceeds the
//!   400k production budget still *executes* and is measured — the assertion, not
//!   the runtime limit, is the regression gate.
//! - The reported CU includes the compute-budget instruction's own fixed cost,
//!   which production pays identically — so the comparison is apples-to-apples.
//! - ALTs compress account *addresses* in the serialized message (tx *size* is a
//!   separate concern); execution CU is unaffected, so a legacy tx here measures
//!   the production CU faithfully.
//! - LiteSVM uses the mock SpokePool/swap, lighter than real Across/an on-chain
//!   DEX, so these understate the real fill and swap legs — conservative for a
//!   regression floor (the true production ceiling requires a real-Across E2E).

mod common;

use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::AccountMeta, pubkey::Pubkey,
    signature::Keypair, signer::Signer,
};
use sra_common::types::{CreateRouteParams, ExecutionStatus, SolanaAction};

/// The compute-unit budget the production destination fill tx requests — design
/// parity with the source deposit, which requests the same 400k.
/// Keep this as an alias so destination assertions track any future budget change.
const DEST_CU_BUDGET: u32 = SOURCE_CU_BUDGET;
/// Runtime CU limit for measurement: the Solana maximum, so the 400k assertion is
/// the gate — a flow over budget still runs and reports its real consumption
/// instead of aborting on the runtime limit.
const MEASUREMENT_CU_LIMIT: u32 = 1_400_000;

const AMOUNT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const POOL_LIQUIDITY: u64 = 100_000_000;
const DECIMALS: u8 = 6;
/// Matches `sample_relay_data`'s fill deadline (the relay hash is computed over it).
const FILL_DEADLINE: u32 = 1_900_000_000;

fn assert_under_budget(consumed: u64, label: &str) {
    // `DEST_CU_BUDGET` is the runtime CU-limit type (`u32`); widen losslessly to
    // compare against the `u64` `compute_units_consumed`.
    let budget = u64::from(DEST_CU_BUDGET);
    let headroom = budget.saturating_sub(consumed);
    println!("destination flow CU [{label}]: {consumed} / {budget} (headroom {headroom})");
    assert!(
        consumed < budget,
        "destination flow consumed {consumed} CU [{label}], exceeds the {budget} CU \
         production budget",
    );
}

/// A route created and filled by the relayer (route vault funded, FillStatus PDA
/// present) — ready for `verify_across_fill`, not yet verified or finalized.
struct Staged {
    route: Pubkey,
    route_config_hash: [u8; 32],
    /// The Across relay hash, which is also the handler-side `fill_id`.
    relay_hash: [u8; 32],
    verified_fill: Pubkey,
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    mint: Pubkey,
}

/// Create a route committing `action_config` (with `payer` as the allowed
/// executor) and have a relayer fill its vault with `AMOUNT` exactly as the
/// deployed Across program does — leaving the FillStatus PDA in place for verify.
fn stage_filled(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    action_config: Vec<SolanaAction>,
    salt: [u8; 32],
) -> Staged {
    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: mint,
        action_config,
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
        allowed_executors: vec![payer.pubkey()],
    };
    let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &salt);
    send_ix(
        svm,
        create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            route_config_hash,
            params,
            &[ADAPTER_PROGRAM_ID],
        ),
        payer,
        &[owner],
    )
    .expect("create_route should succeed");

    let route_vault = derive_ata(&route, &mint);
    let relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let relayer = funded_keypair(svm, 100 * SOL);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    mint_to(svm, payer, payer, &mint, &relayer_ata, AMOUNT);
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
    .expect("relayer fill should succeed");

    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);
    let (fill_receipt, _) = derive_fill_receipt_pda(&route, &relay_hash);

    Staged {
        route,
        route_config_hash,
        relay_hash,
        verified_fill,
        execution_vault,
        fill_receipt,
        mint,
    }
}

impl Staged {
    /// `verify_across_fill` over this fill (`relay_data` is deterministic, so it is
    /// recomputed from the same inputs `stage_filled` used).
    fn verify_ix(&self, payer: &Pubkey) -> solana_sdk::instruction::Instruction {
        let relay_data = sample_relay_data(self.route, self.mint, SOURCE_CHAIN_ID, AMOUNT);
        verify_across_fill_ix(payer, &self.route, self.relay_hash, relay_data)
    }

    /// `finalize_fill` over this fill.
    fn finalize_ix(&self, payer: &Pubkey) -> solana_sdk::instruction::Instruction {
        finalize_fill_ix(
            payer,
            &self.route,
            self.route_config_hash,
            &self.verified_fill,
            self.relay_hash,
            &self.mint,
        )
    }
}

fn cu_limit_ix() -> solana_sdk::instruction::Instruction {
    ComputeBudgetInstruction::set_compute_unit_limit(MEASUREMENT_CU_LIMIT)
}

#[test]
fn dest_2ix_flow_under_cu_budget() {
    // The production destination fill tx: verify_across_fill + finalize_fill.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Pubkey::new_unique();
    let s = stage_filled(
        &mut svm,
        &payer,
        &owner,
        mint,
        vec![spl(recipient, 10_000)],
        [0x10; 32],
    );

    let consumed = send_instructions(
        &mut svm,
        &[
            cu_limit_ix(),
            s.verify_ix(&payer.pubkey()),
            s.finalize_ix(&payer.pubkey()),
        ],
        &payer,
        &[],
    )
    .expect("destination 2-IX flow must execute")
    .compute_units_consumed;

    // Prove the flow actually ran, so a degenerate (cheap, no-op) tx can't slip
    // under the budget as a false green.
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), AMOUNT);
    assert_under_budget(consumed, "2-IX verify + finalize");
}

#[test]
fn dest_finalize_execute_spl_under_cu_budget() {
    // finalize_fill + execute_actions combined, at the maximum SplTransfer fan-out.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);

    // Four recipients (the action bound), bps summing to 100% so the vault drains
    // (AMOUNT is divisible by 4, so the floor splits leave no dust).
    let recipients: Vec<Pubkey> = (0..4).map(|_| Pubkey::new_unique()).collect();
    let cfg: Vec<SolanaAction> = recipients.iter().map(|r| spl(*r, 2_500)).collect();
    let s = stage_filled(&mut svm, &payer, &owner, mint, cfg, [0x11; 32]);

    // verify is the separately-measured 2-IX leg, so it is setup here. The
    // recipient ATAs must exist so execute takes the heavy transfer path (a
    // missing ATA would soft-fail with no transfers — a cheaper, unrepresentative
    // measurement).
    send_ix(&mut svm, s.verify_ix(&payer.pubkey()), &payer, &[]).expect("verify (setup)");
    let recipient_atas: Vec<Pubkey> = recipients
        .iter()
        .map(|r| create_ata(&mut svm, &payer, r, &mint))
        .collect();

    let consumed = send_instructions(
        &mut svm,
        &[
            cu_limit_ix(),
            s.finalize_ix(&payer.pubkey()),
            execute_actions_ix(
                &payer.pubkey(),
                &s.route,
                s.relay_hash,
                &mint,
                &recipient_atas,
            ),
        ],
        &payer,
        &[],
    )
    .expect("finalize + execute (4x SplTransfer) must execute")
    .compute_units_consumed;

    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    for ata in &recipient_atas {
        assert_eq!(fetch_token_balance(&svm, ata), AMOUNT / 4);
    }
    assert_under_budget(consumed, "finalize + execute_actions (4x SplTransfer)");
}

#[test]
fn dest_finalize_execute_verified_cpi_under_cu_budget() {
    // finalize_fill + execute_actions combined, for the heaviest action: a
    // verified-CPI swap through the shared engine, co-signed by the
    // route-committed quoter.
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let quoter = Keypair::new();
    let input_mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);

    let cfg = vec![SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps: 10_000,
        account_template: mock_swap_template(),
        output_mint,
        quoter: quoter.pubkey(),
        allow_permissioned_output: false,
    }];
    let s = stage_filled(&mut svm, &payer, &owner, input_mint, cfg, [0x12; 32]);

    // verify is setup; stand up the scratch ATA, the owner's output destination,
    // and a funded mock-swap pool.
    send_ix(&mut svm, s.verify_ix(&payer.pubkey()), &payer, &[]).expect("verify (setup)");
    let (scratch_authority, _) = derive_scratch_authority_pda(&s.route);
    let scratch = create_ata(&mut svm, &payer, &scratch_authority, &input_mint);
    let destination = create_ata(&mut svm, &payer, &owner.pubkey(), &output_mint);
    let (pool_authority, pool_input, pool_output) =
        derive_mock_swap_pool(&input_mint, &output_mint);
    create_ata(&mut svm, &payer, &pool_authority, &input_mint);
    create_ata(&mut svm, &payer, &pool_authority, &output_mint);
    mint_to(
        &mut svm,
        &payer,
        &payer,
        &output_mint,
        &pool_output,
        POOL_LIQUIDITY,
    );

    let out_amount = 3_000_000;
    let min_out = 2_900_000;
    let expiry = now(&svm) + 3_600;

    // The per-action `[program, <10 template slots>]` metas the engine resolves
    // (legacy output token program), mirroring the shared mock-swap template.
    let verified_metas = vec![
        AccountMeta::new_readonly(mock_swap::ID, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        AccountMeta::new_readonly(scratch_authority, false),
        AccountMeta::new(scratch, false),
        AccountMeta::new(destination, false),
        AccountMeta::new_readonly(output_mint, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        AccountMeta::new_readonly(input_mint, false),
        AccountMeta::new_readonly(pool_authority, false),
        AccountMeta::new(pool_input, false),
        AccountMeta::new(pool_output, false),
    ];
    let exec = execute_actions_ix_full(
        &payer.pubkey(),
        &s.route,
        s.relay_hash,
        vec![runtime(swap_data(AMOUNT, out_amount), min_out)],
        expiry,
        Some(quoter.pubkey()),
        &input_mint,
        verified_metas,
    );

    let consumed = send_instructions(
        &mut svm,
        &[cu_limit_ix(), s.finalize_ix(&payer.pubkey()), exec],
        &payer,
        &[&quoter],
    )
    .expect("finalize + execute (verified-CPI swap) must execute")
    .compute_units_consumed;

    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(fetch_token_balance(&svm, &destination), out_amount);
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), 0);
    assert_eq!(
        fetch_token_balance(&svm, &scratch),
        0,
        "scratch swept empty"
    );
    assert_under_budget(consumed, "finalize + execute_actions (verified-CPI swap)");
}
