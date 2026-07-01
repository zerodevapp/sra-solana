//! Adversarial destination-action tests — the security invariants of the
//! execution path, driven against hostile inputs and runtime states.
//!
//! The per-instruction suites prove each individual check. This suite consolidates
//! the *attack* angles into one place and pins the two properties an attacker cares
//! about: every rejection (1) preserves the staged funds and (2) leaves the receipt
//! in a recoverable/retryable state. It also pins the fund-safety invariants that
//! span instructions — the bps drain bound, snapshot determinism under an
//! unsolicited top-up, the scratch isolation that bounds a relayer-supplied account
//! set, and the stranded-funds inventory (every PDA-owned token account has an
//! owner-reachable exit from every pre-terminal state).
//!
//! EVM-parity note: the staged-funds-are-bounded / atomic / fund-safe model mirrors
//! `_triggerTargetAction` (cap to balance, OOG-protected, marks-processed) and the
//! single-use `processedMessages` replay marker. The scratch / pinned-slot / signed-
//! quote isolation is the Solana-forced safety model for handing a relayer account
//! set to a PDA-signed CPI — it has no EVM analogue but contradicts none of it.

mod common;

use anchor_spl::token::ID as TOKEN_PROGRAM_ID;
use common::*;
use solana_sdk::{instruction::AccountMeta, pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_common::{
    errors::SraError,
    quote::QuotedAction,
    types::{CreateRouteParams, ExecutionStatus, MinOut, SolanaAction},
};

const VAULT: u64 = 4_000_000;
const SOURCE_CHAIN_ID: u64 = 42_161;
const DECIMALS: u8 = 6;
const POOL_LIQUIDITY: u64 = 100_000_000;
/// Matches `sample_relay_data`'s fill deadline (the relay hash is computed over it).
const FILL_DEADLINE: u32 = 1_900_000_000;

// ==========================================================================
// Genuine-finalize driver (the real relayer fill -> verify -> finalize entry)
// ==========================================================================

/// A route driven through the genuine finalize path: `AMOUNT` of `mint` is staged
/// in the execution vault and the receipt is `Finalized`.
struct FinalizedRoute {
    route: Pubkey,
    fill_id: [u8; 32],
    route_config_hash: [u8; 32],
    execution_vault: Pubkey,
    route_vault: Pubkey,
    fill_receipt: Pubkey,
}

/// Create a route committing `action_config`, then drive the genuine finalize path
/// (relayer fill -> adapter `verify_across_fill` -> `finalize_fill`) so the receipt
/// lands in `Finalized` with `amount` of `mint` staged in the execution vault.
fn finalize_route(
    svm: &mut litesvm::LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    action_config: Vec<SolanaAction>,
    salt: [u8; 32],
    amount: u64,
) -> FinalizedRoute {
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
    let create = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        route_config_hash,
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(svm, create, payer, &[owner]).expect("create_route should succeed");

    let route_vault = derive_ata(&route, &mint);
    let relay_data = sample_relay_data(route, mint, SOURCE_CHAIN_ID, amount);
    let relay_hash = relay_hash_for(&relay_data);
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
        verify_across_fill_ix(&payer.pubkey(), &route, relay_hash, relay_data),
        payer,
        &[],
    )
    .expect("verify_across_fill should succeed");
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    send_ix(
        svm,
        finalize_fill_ix(
            &payer.pubkey(),
            &route,
            route_config_hash,
            &verified_fill,
            relay_hash,
            &mint,
        ),
        payer,
        &[],
    )
    .expect("finalize_fill should succeed");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &mint);
    let (fill_receipt, _) = derive_fill_receipt_pda(&route, &relay_hash);

    assert_eq!(fetch_token_balance(svm, &execution_vault), amount);
    assert_eq!(status(svm, &fill_receipt), ExecutionStatus::Finalized);

    FinalizedRoute {
        route,
        fill_id: relay_hash,
        route_config_hash,
        execution_vault,
        route_vault,
        fill_receipt,
    }
}

// ==========================================================================
// SplTransfer adversarial — drain bound, commitment defense, soft-fail recovery
// ==========================================================================

/// Each `SplTransfer` amount is a fraction of one start-of-run vault snapshot, and
/// `create_route` caps Σ `SplTransfer.amount_bps` ≤ 100% — so a full-allocation
/// SplTransfer route distributes exactly the snapshot (minus floor dust), never more.
/// (A combined over-commitment that spans variants is not caught at create time but
/// is fail-closed at execution — see
/// `mixed_variant_combined_draw_over_100pct_reverts_funds_safe`.) Serves the same
/// over-draw-safety goal as the EVM cap-to-balance, by a snapshot×bps mechanism.
#[test]
fn action_drain_at_full_bps_drains_vault_without_overflow() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);

    // Four recipients splitting the whole vault: 2500 + 2500 + 2500 + 2500 = 10000.
    let recipients: Vec<Keypair> = (0..4).map(|_| Keypair::new()).collect();
    let atas: Vec<Pubkey> = recipients
        .iter()
        .map(|r| create_ata(&mut svm, &payer, &r.pubkey(), &mint))
        .collect();
    let action_config: Vec<SolanaAction> =
        recipients.iter().map(|r| spl(r.pubkey(), 2_500)).collect();

    let fr = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        action_config.clone(),
        [7u8; 32],
        VAULT,
    );

    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr.route,
            fr.fill_id,
            &mint,
            &atas,
            0,
        ),
        &payer,
        &[],
    )
    .expect("full-bps drain should succeed");

    let distributed: u64 = atas.iter().map(|a| fetch_token_balance(&svm, a)).sum();
    assert_eq!(distributed, VAULT, "the full vault is distributed, no more");
    assert_eq!(
        fetch_token_balance(&svm, &fr.execution_vault),
        0,
        "vault fully drained at Σbps == 100%"
    );
    assert_eq!(
        status(&svm, &fr.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// A frozen recipient ATA is a runtime state the owner cannot prevent at create
/// time. It must soft-fail to `ActionsFailed` (funds staged, not aborted-with-
/// access-lost) and the owner must retain the recovery exit.
#[test]
fn recipient_ata_frozen_soft_fails_then_recovers() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Keypair::new();
    let recipient_ata = create_ata(&mut svm, &payer, &recipient.pubkey(), &mint);
    freeze_token_account(&mut svm, &recipient_ata);

    let action_config = vec![spl(recipient.pubkey(), 10_000)];
    let fr = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        action_config.clone(),
        [10u8; 32],
        VAULT,
    );

    // Execute: the frozen recipient ATA -> soft ActionsFailed, no transfer.
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr.route,
            fr.fill_id,
            &mint,
            &[recipient_ata],
            0,
        ),
        &payer,
        &[],
    )
    .expect("execute returns Ok with a soft failure");
    assert_eq!(
        status(&svm, &fr.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    assert_eq!(
        fetch_token_balance(&svm, &fr.execution_vault),
        VAULT,
        "funds stay staged on a soft failure"
    );
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);

    // The owner recovers the staged funds — the exit from ActionsFailed.
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &fr.route, &fr.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("recover from ActionsFailed should succeed");
    assert_eq!(status(&svm, &fr.fill_receipt), ExecutionStatus::Recovered);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), VAULT);
    assert_eq!(fetch_token_balance(&svm, &fr.execution_vault), 0);
}

/// bps amounts are a fraction of a single start-of-run snapshot of the *current*
/// vault balance — so an unsolicited top-up (tokens donated to the execution vault
/// after finalize) is folded into the snapshot deterministically: a Σbps == 100%
/// config distributes the donation along with the bridged amount, never strands it
/// and never miscomputes.
#[test]
fn unsolicited_topup_of_execution_vault_distributed_deterministically() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Keypair::new();
    let recipient_ata = create_ata(&mut svm, &payer, &recipient.pubkey(), &mint);

    let action_config = vec![spl(recipient.pubkey(), 10_000)];
    let fr = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        action_config.clone(),
        [11u8; 32],
        VAULT,
    );

    // Donate extra tokens directly into the execution vault (unsolicited).
    let donation = 1_234_567u64;
    mint_to(
        &mut svm,
        &payer,
        &payer,
        &mint,
        &fr.execution_vault,
        donation,
    );
    assert_eq!(
        fetch_token_balance(&svm, &fr.execution_vault),
        VAULT + donation
    );

    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr.route,
            fr.fill_id,
            &mint,
            &[recipient_ata],
            0,
        ),
        &payer,
        &[],
    )
    .expect("execute over the topped-up snapshot should succeed");

    // The whole snapshot (bridged + donation) is distributed at Σbps == 100%.
    assert_eq!(
        fetch_token_balance(&svm, &recipient_ata),
        VAULT + donation,
        "donation distributed deterministically with the snapshot"
    );
    assert_eq!(fetch_token_balance(&svm, &fr.execution_vault), 0);
    assert_eq!(
        status(&svm, &fr.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

// ==========================================================================
// Replay / single-use receipt — the Solana analogue of EVM processedMessages
// ==========================================================================

/// The fill receipt is a single-use marker. Solana's account-lock model rules out
/// a concurrent finalize+execute interleave within a slot; across slots the marker
/// enforces it: a second `finalize_fill` of the same fill collides on the `init`
/// PDA, and `execute_actions` on an already-acted receipt is rejected — funds can
/// never be double-distributed.
#[test]
fn double_finalize_rejected_and_execute_after_success_rejected() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Keypair::new();
    let recipient_ata = create_ata(&mut svm, &payer, &recipient.pubkey(), &mint);

    let action_config = vec![spl(recipient.pubkey(), 10_000)];
    let fr = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        action_config.clone(),
        [12u8; 32],
        VAULT,
    );

    // A second finalize of the same fill collides on the `init` receipt PDA.
    let (verified_fill, _) = derive_verified_fill_pda(&fr.fill_id);
    let second_finalize = finalize_fill_ix(
        &payer.pubkey(),
        &fr.route,
        fr.route_config_hash,
        &verified_fill,
        fr.fill_id,
        &mint,
    );
    assert!(
        send_ix(&mut svm, second_finalize, &payer, &[]).is_err(),
        "a second finalize of the same fill must collide on the receipt PDA"
    );
    // The receipt is unchanged and the funds are still staged exactly once.
    assert_eq!(status(&svm, &fr.fill_receipt), ExecutionStatus::Finalized);
    assert_eq!(fetch_token_balance(&svm, &fr.execution_vault), VAULT);

    // Execute once -> ActionsSucceeded.
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr.route,
            fr.fill_id,
            &mint,
            &[recipient_ata],
            0,
        ),
        &payer,
        &[],
    )
    .expect("first execute should succeed");
    assert_eq!(
        status(&svm, &fr.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), VAULT);

    // A second execute on the now-terminal receipt is rejected; no second payout.
    let err = send_ix_expect_err(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr.route,
            fr.fill_id,
            &mint,
            &[recipient_ata],
            0,
        ),
        &payer,
        &[],
        "re-execute terminal receipt",
    );
    assert_eq!(err, custom_error(SraError::InvalidExecutionStatus));
    assert_eq!(
        fetch_token_balance(&svm, &recipient_ata),
        VAULT,
        "no double payout"
    );
}

// ==========================================================================
// Stranded-funds inventory — every PDA-owned token account has an exit path
// ==========================================================================

/// Inventory of the destination's PDA-owned token accounts and the owner-reachable
/// exit from every pre-terminal state. This is the funds-never-trapped checklist as
/// an executable test.
///
/// * **route_vault** (Across fill lands here): `finalize_fill` moves the full
///   verified fill amount out, so a protocol-placed fill is evacuated in full. (The
///   only residue a route_vault can hold is tokens transferred directly to the ATA
///   out-of-band — a burn-to-address donation the protocol never places there and,
///   like any such donation, has no spending path. Not a protocol strand.)
/// * **execution_vault @ Finalized**: exits via `recover_funds` (drain) or
///   `execute_actions` (distribute).
/// * **execution_vault @ ActionsFailed**: exits via `recover_funds` or
///   `retry_actions`.
/// * **execution_vault @ ActionsSucceeded / Recovered**: terminal — the owner's
///   committed distribution (or recovery) already ran; a Σbps == 100% route leaves
///   zero residue. (A deliberately under-100%/empty `action_config` leaves
///   owner-committed residue that recovery rejects on the terminal status — the
///   owner's create-time choice, pinned by
///   `actions_lifecycle::lifecycle_empty_action_config_actions_succeeded_immediately`,
///   not a protocol strand.)
/// * **scratch** (VerifiedCpi input bound): never holds funds across instructions —
///   `execute_verified` sweeps it to zero or the tx reverts (`ScratchNotEmpty`),
///   pinned by `verified_cpi_prefunded_scratch_swept_to_vault` below.
#[test]
fn stranded_funds_inventory_holds() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let owner = funded_keypair(&mut svm, 100 * SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), DECIMALS);
    let recipient = Keypair::new();

    // --- route_vault: fully evacuated by finalize_fill ---
    let action_config = vec![spl(recipient.pubkey(), 10_000)];
    let fr = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        action_config.clone(),
        [20u8; 32],
        VAULT,
    );
    assert_eq!(
        fetch_token_balance(&svm, &fr.route_vault),
        0,
        "route_vault fully evacuated by finalize"
    );
    assert_eq!(fetch_token_balance(&svm, &fr.execution_vault), VAULT);

    // --- execution_vault @ Finalized: recover drains to the owner ---
    let owner_ata = create_ata(&mut svm, &payer, &owner.pubkey(), &mint);
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &fr.route, &fr.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("Finalized -> recover exit");
    assert_eq!(status(&svm, &fr.fill_receipt), ExecutionStatus::Recovered);
    assert_eq!(fetch_token_balance(&svm, &owner_ata), VAULT);
    assert_eq!(
        fetch_token_balance(&svm, &fr.execution_vault),
        0,
        "execution_vault drained from Finalized"
    );

    // --- execution_vault @ ActionsFailed: recover drains to the owner ---
    let frozen_recipient = Keypair::new();
    let frozen_ata = create_ata(&mut svm, &payer, &frozen_recipient.pubkey(), &mint);
    freeze_token_account(&mut svm, &frozen_ata);
    let failed_config = vec![spl(frozen_recipient.pubkey(), 10_000)];
    let fr2 = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        failed_config.clone(),
        [21u8; 32],
        VAULT,
    );
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr2.route,
            fr2.fill_id,
            &mint,
            &[frozen_ata],
            0,
        ),
        &payer,
        &[],
    )
    .expect("execute soft-fails");
    assert_eq!(
        status(&svm, &fr2.fill_receipt),
        ExecutionStatus::ActionsFailed
    );
    send_ix(
        &mut svm,
        recover_funds_ix(&owner.pubkey(), &fr2.route, &fr2.fill_id, &mint, &owner_ata),
        &owner,
        &[],
    )
    .expect("ActionsFailed -> recover exit");
    assert_eq!(status(&svm, &fr2.fill_receipt), ExecutionStatus::Recovered);
    assert_eq!(
        fetch_token_balance(&svm, &owner_ata),
        VAULT * 2,
        "both recoveries credited the owner"
    );

    // --- execution_vault @ ActionsSucceeded: a Σbps == 100% route leaves no residue ---
    let succ_recipient = Keypair::new();
    let succ_ata = create_ata(&mut svm, &payer, &succ_recipient.pubkey(), &mint);
    let succ_config = vec![spl(succ_recipient.pubkey(), 10_000)];
    let fr3 = finalize_route(
        &mut svm,
        &payer,
        &owner,
        mint,
        succ_config.clone(),
        [22u8; 32],
        VAULT,
    );
    send_ix(
        &mut svm,
        execute_actions_ix(
            &payer.pubkey(),
            &fr3.route,
            fr3.fill_id,
            &mint,
            &[succ_ata],
            0,
        ),
        &payer,
        &[],
    )
    .expect("execute succeeds");
    assert_eq!(
        status(&svm, &fr3.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
    assert_eq!(
        fetch_token_balance(&svm, &fr3.execution_vault),
        0,
        "no residue stranded on a fully-allocated terminal route"
    );
}

// ==========================================================================
// VerifiedCpi adversarial — staging mirrors verified_cpi_actions.rs
// ==========================================================================

/// A staged VerifiedCpi route (planted `Finalized` receipt + mock-swap pool + scratch
/// + destination), enough to drive the engine against hostile account sets.
struct StagedVerified {
    payer: Keypair,
    input_mint: Pubkey,
    output_mint: Pubkey,
    route: Pubkey,
    fill_id: [u8; 32],
    execution_vault: Pubkey,
    fill_receipt: Pubkey,
    scratch_authority: Pubkey,
    scratch: Pubkey,
    destination: Pubkey,
    pool_authority: Pubkey,
    pool_input: Pubkey,
    pool_output: Pubkey,
}

const V_FILL_ID: [u8; 32] = [33u8; 32];

fn verified_cpi(
    input_mint: Pubkey,
    output_mint: Pubkey,
    input_bps: u16,
    quoter: Pubkey,
) -> SolanaAction {
    SolanaAction::VerifiedCpi {
        program_id: mock_swap::ID,
        input_mint,
        input_bps,
        account_template: mock_swap_template(),
        output_mint,
        min_out: MinOut::SignedQuote { quoter },
        allow_permissioned_output: false,
    }
}

/// Stage a legacy-output route whose `action_config` is built from
/// `(input_mint, output_mint, quoter)` by `make_actions`, with the execution vault
/// funded with `VAULT`, the mock-swap pool funded, scratch + destination created, and
/// a `Finalized` receipt planted. (The single-VerifiedCpi tests pass a one-action
/// closure; the mixed-variant test prepends an `SplTransfer`.)
fn stage_verified(
    svm: &mut litesvm::LiteSVM,
    quoter: &Keypair,
    make_actions: impl FnOnce(Pubkey, Pubkey, Pubkey) -> Vec<SolanaAction>,
) -> StagedVerified {
    let payer = funded_keypair(svm, 100 * SOL);
    let owner = funded_keypair(svm, 100 * SOL);
    let input_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let output_mint = create_mint(svm, &payer, &payer.pubkey(), DECIMALS);
    let action_config = make_actions(input_mint, output_mint, quoter.pubkey());
    let salt = [3u8; 32];

    let params = CreateRouteParams {
        source_chain_id: SOURCE_CHAIN_ID,
        expected_mint: input_mint,
        action_config: action_config.clone(),
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
        allowed_executors: vec![payer.pubkey()],
    };
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &hash, &salt);
    send_ix(
        svm,
        create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            hash,
            params,
            &[ADAPTER_PROGRAM_ID],
        ),
        &payer,
        &[&owner],
    )
    .expect("create_route should succeed");

    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let execution_vault = derive_ata(&execution_authority, &input_mint);
    mint_to(svm, &payer, &payer, &input_mint, &execution_vault, VAULT);

    let (scratch_authority, _) = derive_scratch_authority_pda(&route);
    let scratch = create_ata(svm, &payer, &scratch_authority, &input_mint);
    let destination = create_ata(svm, &payer, &owner.pubkey(), &output_mint);

    let (pool_authority, _) =
        Pubkey::find_program_address(&[mock_swap::POOL_AUTHORITY_SEED], &mock_swap::ID);
    let pool_input = create_ata(svm, &payer, &pool_authority, &input_mint);
    let pool_output = create_ata(svm, &payer, &pool_authority, &output_mint);
    mint_to(
        svm,
        &payer,
        &payer,
        &output_mint,
        &pool_output,
        POOL_LIQUIDITY,
    );

    let fill_receipt =
        seed_fill_receipt_with_status(svm, &route, V_FILL_ID, VAULT, ExecutionStatus::Finalized);

    StagedVerified {
        payer,
        input_mint,
        output_mint,
        route,
        fill_id: V_FILL_ID,
        execution_vault,
        fill_receipt,
        scratch_authority,
        scratch,
        destination,
        pool_authority,
        pool_input,
        pool_output,
    }
}

impl StagedVerified {
    /// `[program, <10 template slots>]` for the single VerifiedCpi action. Each
    /// pinned slot is overridable to model a relayer substituting it.
    fn metas(
        &self,
        program: Pubkey,
        signing_authority: Pubkey,
        source: Pubkey,
        destination: Pubkey,
    ) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(program, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // TokenProgram (input)
            AccountMeta::new_readonly(signing_authority, false), // SigningAuthority
            AccountMeta::new(source, false),                    // Source (scratch)
            AccountMeta::new(destination, false),               // Destination
            AccountMeta::new_readonly(self.output_mint, false), // OutputMint
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false), // Free: output token program
            AccountMeta::new_readonly(self.input_mint, false),  // Free: input mint
            AccountMeta::new_readonly(self.pool_authority, false),
            AccountMeta::new(self.pool_input, false),
            AccountMeta::new(self.pool_output, false),
        ]
    }

    /// The honest meta set (every pinned slot resolved correctly).
    fn honest_metas(&self) -> Vec<AccountMeta> {
        self.metas(
            mock_swap::ID,
            self.scratch_authority,
            self.scratch,
            self.destination,
        )
    }

    /// The signed-quote tuple for the fill's single VerifiedCpi action.
    fn quoted_action(&self, input_amount: u64, min_out: u64) -> QuotedAction {
        QuotedAction {
            input_mint: self.input_mint,
            output_mint: self.output_mint,
            input_amount,
            min_out,
        }
    }

    /// The aggregate signed-quote instruction bound to this route/fill/expiry.
    fn agg_quote_ix(
        &self,
        quoter: &Keypair,
        expiry: i64,
        actions: &[QuotedAction],
    ) -> solana_sdk::instruction::Instruction {
        aggregate_quote_ix(quoter, &self.route, &self.fill_id, expiry, actions)
    }
}

/// `exec_error` for the VerifiedCpi path: the execute instruction sits at index 1
/// (the ed25519 quote is index 0).
fn verified_exec_error(err: SraError) -> solana_sdk::transaction::TransactionError {
    use solana_sdk::instruction::InstructionError;
    solana_sdk::transaction::TransactionError::InstructionError(
        1,
        InstructionError::Custom(err.into()),
    )
}

/// The committed CPI target must be a deployed, executable, loader-owned program —
/// not just the right pubkey. Here the account *at the committed program id* is
/// overwritten with a non-executable, system-owned account (the identity check
/// passes, so the executable/loader-owned half of the pin is what must fire). The
/// CPI never happens; funds are untouched and the receipt stays re-executable.
#[test]
fn malicious_cpi_target_rejected_funds_safe() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![verified_cpi(i, o, 10_000, q)]
    });
    // Replace the deployed mock-swap program with a non-executable account at the
    // SAME committed id, so the pin's identity check passes and the
    // executable/loader-owned check is the one that rejects.
    plant_non_executable_at(&mut svm, mock_swap::ID);

    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.honest_metas(),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, verified_exec_error(SraError::ProgramNotPinned));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// A relayer cannot point the engine's Source (scratch) slot at a foreign token
/// account — the engine derives the scratch from the route and pins it. Substitution
/// is a hard reject; the vault is untouched.
#[test]
fn verified_cpi_source_slot_substitution_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![verified_cpi(i, o, 10_000, q)]
    });
    // A token account the relayer controls (same mint), offered as the input source.
    let attacker = Keypair::new();
    let attacker_ata = create_ata(&mut svm, &s.payer, &attacker.pubkey(), &s.input_mint);

    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.metas(
            mock_swap::ID,
            s.scratch_authority,
            attacker_ata,
            s.destination,
        ),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, verified_exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// A relayer cannot point the SigningAuthority slot at a key it controls — the
/// engine pins it to the route's scratch authority PDA. Substitution is a hard
/// reject; the vault is untouched.
#[test]
fn verified_cpi_signing_authority_substitution_rejected() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![verified_cpi(i, o, 10_000, q)]
    });
    let attacker_authority = Pubkey::new_unique();

    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1_000_000)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(VAULT, 1_000_000), 1_000_000)],
        &s.input_mint,
        s.metas(mock_swap::ID, attacker_authority, s.scratch, s.destination),
        expiry,
    );

    let err = send_instructions_expect_err(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert_eq!(err, verified_exec_error(SraError::ActionAccountMismatch));
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}

/// The scratch bound caps the spend at `input_bps` of the vault, no matter how
/// greedy the inner program or how the relayer routes the Free slots: only the
/// scratch is exposed to the CPI, and the scratch holds at most `input_bps` of the
/// snapshot. Here `input_bps == 50%` and the swap tries to pull the entire scratch —
/// the vault still retains exactly the other 50%.
#[test]
fn verified_cpi_hostile_account_set_bounded_by_input_bps() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![verified_cpi(i, o, 5_000, q)]
    });
    let input_amount = VAULT / 2; // 50% of the snapshot is moved into scratch.

    let expiry = now(&svm) + 3_600;
    let out_amount = 1_000_000;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(input_amount, out_amount)]);
    // The inner swap is told to pull the full scratch balance — the maximum a
    // hostile account set could attempt.
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(input_amount, out_amount), out_amount)],
        &s.input_mint,
        s.honest_metas(),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("bounded swap succeeds");
    assert_eq!(
        fetch_token_balance(&svm, &s.execution_vault),
        VAULT - input_amount,
        "at most input_bps of the vault is ever exposed"
    );
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch swept empty"
    );
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// The scratch always ends empty. Tokens donated to the scratch ATA before a
/// VerifiedCpi run are swept back into the execution vault along with the unspent
/// input — never stranded in the scratch, never reachable by the inner program
/// beyond the bounded input. The post-run `require!(scratch == 0)` is the gate.
#[test]
fn verified_cpi_prefunded_scratch_swept_to_vault() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![verified_cpi(i, o, 10_000, q)]
    });

    // Donate extra input tokens directly into the scratch ATA (unsolicited).
    let donation = 777_000u64;
    mint_to(
        &mut svm,
        &s.payer,
        &s.payer,
        &s.input_mint,
        &s.scratch,
        donation,
    );
    assert_eq!(fetch_token_balance(&svm, &s.scratch), donation);

    // A full-vault swap that pulls only the bridged input; the donation must sweep
    // back to the vault and the scratch must end at zero.
    let pulled = VAULT;
    let out_amount = 1_000_000;
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, out_amount)]);
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(pulled, out_amount), out_amount)],
        &s.input_mint,
        s.honest_metas(),
        expiry,
    );

    send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]).expect("swap succeeds");
    assert_eq!(
        fetch_token_balance(&svm, &s.scratch),
        0,
        "scratch ends empty even when pre-funded"
    );
    assert_eq!(
        fetch_token_balance(&svm, &s.execution_vault),
        donation,
        "the donation is swept into the vault, recoverable by the owner"
    );
    assert_eq!(fetch_token_balance(&svm, &s.destination), out_amount);
    assert_eq!(
        status(&svm, &s.fill_receipt),
        ExecutionStatus::ActionsSucceeded
    );
}

/// A combined over-commitment that spans action variants — `SplTransfer{100%}` plus
/// `VerifiedCpi{input_bps:100%}`, each a fraction of the same snapshot — is not summed
/// at `create_route` (only `SplTransfer` bps are summed there), so the route is
/// creatable. It is fail-closed at execution instead: the two-pass executor validates
/// and plans both, then in pass 2 the `SplTransfer` drains the vault and the
/// `VerifiedCpi`'s vault->scratch move runs short and reverts the whole instruction
/// atomically. No partial transfer persists; funds stay staged, receipt `Finalized`.
#[test]
fn mixed_variant_combined_draw_over_100pct_reverts_funds_safe() {
    let mut svm = setup_svm();
    let quoter = Keypair::new();
    let recipient = Keypair::new();
    // [ SplTransfer 100%, VerifiedCpi input 100% ] = 200% of the snapshot committed.
    let s = stage_verified(&mut svm, &quoter, |i, o, q| {
        vec![
            spl(recipient.pubkey(), 10_000),
            verified_cpi(i, o, 10_000, q),
        ]
    });
    let recipient_ata = create_ata(&mut svm, &s.payer, &recipient.pubkey(), &s.input_mint);

    // The VerifiedCpi is action index 1; its quote must verify in pass 1 so the run
    // reaches the pass-2 revert (not a quote-failure revert). The aggregate covers
    // only the VerifiedCpi action (the SplTransfer contributes no quote tuple).
    let expiry = now(&svm) + 3_600;
    let ed_ix = s.agg_quote_ix(&quoter, expiry, &[s.quoted_action(VAULT, 1)]);
    let mut remaining = vec![AccountMeta::new(recipient_ata, false)];
    remaining.extend(s.honest_metas());
    let exec = execute_actions_ix_full(
        &s.payer.pubkey(),
        &s.route,
        s.fill_id,
        vec![runtime(swap_data(VAULT, 1), 1)],
        &s.input_mint,
        remaining,
        expiry,
    );

    let res = send_instructions(&mut svm, &[ed_ix, exec], &s.payer, &[]);
    assert!(
        res.is_err(),
        "a combined >100% draw must revert at execution"
    );
    // Atomic rollback: no SplTransfer leg persisted; funds fully staged + retryable.
    assert_eq!(fetch_token_balance(&svm, &s.execution_vault), VAULT);
    assert_eq!(fetch_token_balance(&svm, &recipient_ata), 0);
    assert_eq!(status(&svm, &s.fill_receipt), ExecutionStatus::Finalized);
}
