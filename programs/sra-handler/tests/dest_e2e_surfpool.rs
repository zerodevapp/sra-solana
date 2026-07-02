//! Destination E2E against the REAL Across SVM Spoke Pool on a Surfpool
//! mainnet fork.
//!
//! LiteSVM destination tests exercise every reject path against a mock spoke
//! pool, but the mock is built from the same vendored IDL assumptions the
//! adapter itself encodes: the `FillStatusAccount` layout + discriminator, the
//! `["fills", relay_hash]` PDA seeds, and — above all — the relay-hash
//! preimage (Borsh prefix ‖ message hash ‖ chain id). A drift in ANY of those
//! passes the mock and bricks production. Here a funded test relayer executes
//! the genuine `fill_relay` on the fork, so the `FillStatusAccount` our
//! adapter verifies is created by the real program from the real preimage:
//!
//!   fill_relay (real Across writes FillStatus, pays route vault)
//!     → verify_across_fill + finalize_fill (one v0 tx, real ALT, 400k CU)
//!     → execute_actions          (happy path: committed 60/40 SplTransfer split)
//!     → recover_funds            (recovery path: owner drains execution vault)
//!     → finalize_fill again      (replay: rejected on the FillReceipt while the
//!                                 route vault is funded by a second real fill)
//!
//! Every hop asserts EXACT balance conservation (full-amount forwarding: no
//! destination-side fees; fees are collected on the source deposit), with the
//! amounts deliberately de-aliased: unrelated dust escrowed in the route vault
//! must survive finalize untouched (finalize draws exactly the verified
//! amount, not the vault balance), and the 60/40 split gives each recipient a
//! delta distinct from every vault total.
//!
//! The fill is sent with an EMPTY `message`: that is the production shape for
//! a vault recipient (the real program would otherwise CPI a handler), and it
//! pins the adapter's empty-message convention (zero bytes32, not
//! `keccak256("")`) against the real program's preimage.

mod common;

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_spl::associated_token::spl_associated_token_account;
use common::surfpool::*;
use common::*;
use reqwest::blocking::Client;
use serde_json::json;
use solana_client::rpc_client::RpcClient;
use solana_program::keccak;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signature, Signer},
};
use sra_across_adapter::utils::svm_spoke;
use sra_across_adapter::utils::svm_spoke::types::RelayData;
use sra_common::{
    abi::u256_from_u64,
    constants::{ACROSS_SPOKE_POOL_ID, SOLANA_CHAIN_ID, USDC_MINT},
    types::{CreateRouteParams, ExecutionStatus, FillReceipt},
};

/// One USDC (6 decimals) — the amount the relayer delivers to the route.
const OUTPUT_AMOUNT: u64 = 1_000_000;
const RELAYER_STARTING_BALANCE: u64 = 10_000_000;
/// Matches the route's committed `source_chain_id` (Arbitrum One).
const ORIGIN_CHAIN_ID: u64 = 42_161;
/// How far past the fork's current time the fill deadline sits.
const FILL_DEADLINE_WINDOW_SECS: i64 = 3_600;
/// The route commits a 60/40 SplTransfer split. Distinct per-recipient
/// amounts keep "pay bps of the snapshot" distinguishable from "pay the whole
/// vault" (with a single 100% action every correct and incorrect amount
/// formula produces the same number).
const SPLIT_A_BPS: u16 = 6_000;
const SPLIT_B_BPS: u16 = 4_000;
const SPLIT_A_AMOUNT: u64 = 600_000;
const SPLIT_B_AMOUNT: u64 = 400_000;
/// Unrelated balance seeded into the route vault before finalization. The
/// vault must hold exactly this after `finalize_fill` — pinning that finalize
/// moves exactly the verified fill amount, not the vault's whole balance.
const ROUTE_VAULT_DUST: u64 = 12_345;
/// The replay test's second, larger fill: ≥ the first fill's amount so a
/// hypothetical replay double-draw would actually have funds to steal, and
/// distinct from it so amounts never alias.
const SECOND_FILL_AMOUNT: u64 = 1_500_000;

/// The delegate signer the real `fill_relay` moves relayer funds through:
/// `["delegate", keccak256(borsh(relay_hash ‖ repayment_chain_id ‖
/// repayment_address))]` under the spoke program. The relayer must approve it
/// for `output_amount` in the same transaction.
fn fill_delegate_pda(
    relay_hash: [u8; 32],
    repayment_chain_id: u64,
    repayment_address: Pubkey,
) -> Pubkey {
    let mut preimage = Vec::with_capacity(32 + 8 + 32);
    preimage.extend_from_slice(&relay_hash);
    preimage.extend_from_slice(&repayment_chain_id.to_le_bytes());
    preimage.extend_from_slice(repayment_address.as_ref());
    let seed_hash = keccak::hash(&preimage).to_bytes();
    Pubkey::find_program_address(&[b"delegate", seed_hash.as_ref()], &ACROSS_SPOKE_POOL_ID).0
}

struct SurfpoolDestFixture {
    rpc: RpcClient,
    http: Client,
    payer: Keypair,
    owner: Keypair,
    relayer: Keypair,
    route: Pubkey,
    route_config_hash: [u8; 32],
    route_vault: Pubkey,
    execution_vault: Pubkey,
    relayer_ata: Pubkey,
    owner_ata: Pubkey,
    recipient_a_ata: Pubkey,
    recipient_b_ata: Pubkey,
}

impl SurfpoolDestFixture {
    fn bootstrap() -> TestResult<Self> {
        surfpool_log("bootstrap: starting Surfpool destination fixture setup");
        let rpc = rpc_client();
        wait_for_surfpool(&rpc)?;
        surfpool_log("bootstrap: Surfpool RPC is ready");

        let http = http_client()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let relayer = Keypair::new();
        let recipient_a = Keypair::new().pubkey();
        let recipient_b = Keypair::new().pubkey();

        wait_for_program_deploys(&rpc, &[PROGRAM_ID, ADAPTER_PROGRAM_ID])?;
        surfpool_log("bootstrap: local programs are deployed");
        rpc.get_account(&ACROSS_SPOKE_POOL_ID)?;
        surfpool_log(format!(
            "bootstrap: Across spoke pool {ACROSS_SPOKE_POOL_ID} is reachable"
        ));

        for (label, key, lamports) in [
            ("payer", payer.pubkey(), 10 * SOL),
            ("owner", owner.pubkey(), SOL),
            ("relayer", relayer.pubkey(), SOL),
        ] {
            let signature = rpc.request_airdrop(&key, lamports)?;
            rpc.confirm_transaction(&signature)?;
            surfpool_log(format!(
                "bootstrap: {label} airdrop confirmed with signature {signature}"
            ));
        }

        // A fresh route per bootstrap (fresh owner + salt) — its PDA is the
        // fill recipient, so every test run gets a unique relay hash on the
        // shared fork. The route commits a 60/40 SplTransfer split and allows
        // the payer as executor.
        let salt = Keypair::new().pubkey().to_bytes();
        let params = CreateRouteParams {
            source_chain_id: ORIGIN_CHAIN_ID,
            expected_mint: USDC_MINT,
            action_config: vec![spl(recipient_a, SPLIT_A_BPS), spl(recipient_b, SPLIT_B_BPS)],
            salt,
            allowed_adapters: vec![ADAPTER_PROGRAM_ID],
            allowed_executors: vec![payer.pubkey()],
        };
        let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
        let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &salt);
        let create_route = create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            route_config_hash,
            params,
            &[ADAPTER_PROGRAM_ID],
        );
        send_legacy_transaction(&rpc, "create_route", &payer, &[&owner], &[create_route])?;
        surfpool_log(format!("bootstrap: created route {route}"));

        let (execution_authority, _) = derive_execution_authority_pda(&route);
        let route_vault = derive_ata(&route, &USDC_MINT);
        let execution_vault = derive_ata(&execution_authority, &USDC_MINT);
        let relayer_ata = derive_ata(&relayer.pubkey(), &USDC_MINT);
        let owner_ata = derive_ata(&owner.pubkey(), &USDC_MINT);
        let recipient_a_ata = derive_ata(&recipient_a, &USDC_MINT);
        let recipient_b_ata = derive_ata(&recipient_b, &USDC_MINT);

        // ATAs the flow pays into (and the relayer's funding ATA). The route
        // and execution vaults are created by `create_route` itself.
        let create_atas: Vec<Instruction> = [
            (&relayer.pubkey(), "relayer"),
            (&owner.pubkey(), "owner"),
            (&recipient_a, "action recipient A"),
            (&recipient_b, "action recipient B"),
        ]
        .into_iter()
        .map(|(wallet, _)| {
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &payer.pubkey(),
                wallet,
                &USDC_MINT,
                &anchor_spl::token::ID,
            )
        })
        .collect();
        send_legacy_transaction(&rpc, "create_atas", &payer, &[], &create_atas)?;
        surfpool_log("bootstrap: ensured relayer/owner/recipient ATAs");

        surfpool_rpc(
            &http,
            "surfnet_setTokenAccount",
            json!([
                relayer.pubkey().to_string(),
                USDC_MINT.to_string(),
                { "amount": RELAYER_STARTING_BALANCE },
                anchor_spl::token::ID.to_string()
            ]),
        )?;
        surfpool_log(format!(
            "bootstrap: seeded relayer ATA {relayer_ata} with {RELAYER_STARTING_BALANCE} base units of USDC"
        ));
        surfpool_log("bootstrap: fixture setup complete");

        Ok(Self {
            rpc,
            http,
            payer,
            owner,
            relayer,
            route,
            route_config_hash,
            route_vault,
            execution_vault,
            relayer_ata,
            owner_ata,
            recipient_a_ata,
            recipient_b_ata,
        })
    }

    /// The deposit this fixture's relayer fills: recipient is the route PDA,
    /// output is USDC on this chain, origin matches the route's committed
    /// source chain, and the message is empty (vault recipient — see module
    /// docs). `deposit_id` carries the route address, so it is unique per
    /// bootstrap on the shared fork.
    fn relay_data(&self) -> TestResult<RelayData> {
        self.relay_data_with(self.route.to_bytes(), OUTPUT_AMOUNT)
    }

    /// [`Self::relay_data`] with a caller-chosen `deposit_id` and amount — a
    /// SECOND, distinct deposit against the same route (the replay test fills
    /// two deposits so the route vault is funded when the first fill's
    /// settlement is replayed).
    fn relay_data_with(&self, deposit_id: [u8; 32], output_amount: u64) -> TestResult<RelayData> {
        let now = clock_unix_timestamp(&self.rpc)?;
        Ok(RelayData {
            depositor: Pubkey::new_from_array([0x11; 32]),
            recipient: self.route,
            exclusive_relayer: Pubkey::default(),
            input_token: Pubkey::new_from_array([0x33; 32]),
            output_token: USDC_MINT,
            input_amount: u256_from_u64(output_amount),
            output_amount,
            origin_chain_id: ORIGIN_CHAIN_ID,
            deposit_id,
            fill_deadline: u32::try_from(now + FILL_DEADLINE_WINDOW_SECS)
                .map_err(|_| "fill deadline exceeds u32")?,
            exclusivity_deadline: 0,
            message: vec![],
        })
    }

    /// The relayer executes the REAL `fill_relay` on the fork: approves the
    /// fill delegate for `output_amount`, then fills. The real program
    /// validates the relay hash against its own preimage, moves
    /// `output_amount` from the relayer's ATA to the route vault, and creates
    /// the genuine `FillStatusAccount` at `["fills", relay_hash]`.
    fn execute_real_fill(&self, relay_data: &RelayData) -> TestResult<(Signature, Vec<String>)> {
        let relay_hash = relay_hash_for(relay_data);
        let repayment_chain_id = SOLANA_CHAIN_ID;
        let repayment_address = self.relayer.pubkey();
        let delegate = fill_delegate_pda(relay_hash, repayment_chain_id, repayment_address);
        let (fill_status, _) = derive_fill_status_pda(&relay_hash);
        surfpool_log(format!(
            "execute_real_fill: relay_hash={} fill_status={fill_status} delegate={delegate}",
            solana_sdk::bs58::encode(relay_hash).into_string()
        ));

        let approve_ix = anchor_spl::token::spl_token::instruction::approve(
            &anchor_spl::token::ID,
            &self.relayer_ata,
            &delegate,
            &self.relayer.pubkey(),
            &[],
            relay_data.output_amount,
        )?;
        let fill_ix = Instruction {
            program_id: ACROSS_SPOKE_POOL_ID,
            accounts: svm_spoke::client::accounts::FillRelay {
                signer: self.relayer.pubkey(),
                instruction_params: None,
                state: derive_real_across_state_pda().0,
                delegate,
                mint: USDC_MINT,
                relayer_token_account: self.relayer_ata,
                recipient_token_account: self.route_vault,
                fill_status,
                token_program: anchor_spl::token::ID,
                associated_token_program: spl_associated_token_account::ID,
                system_program: solana_sdk::system_program::ID,
                event_authority: derive_real_across_event_authority_pda().0,
                program: ACROSS_SPOKE_POOL_ID,
            }
            .to_account_metas(None),
            data: svm_spoke::client::args::FillRelay {
                relay_hash,
                relay_data: Some(relay_data.clone()),
                repayment_chain_id: Some(repayment_chain_id),
                repayment_address: Some(repayment_address),
            }
            .data(),
        };

        let signature = send_legacy_transaction(
            &self.rpc,
            "fill_relay",
            &self.relayer,
            &[],
            &[approve_ix, fill_ix],
        )?;
        let logs = logs_for_signature(&self.http, &signature)?;
        surfpool_log(format!(
            "execute_real_fill: fetched {} local log line(s) for signature {signature}",
            logs.len()
        ));
        Ok((signature, logs))
    }

    /// `verify_across_fill` + `finalize_fill` as one v0 transaction with a
    /// real address lookup table and the 400k CU budget request.
    fn verify_and_finalize(&self, relay_data: &RelayData) -> TestResult<[u8; 32]> {
        let relay_hash = relay_hash_for(relay_data);
        let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
        let verify_ix = verify_across_fill_ix(
            &self.payer.pubkey(),
            &self.route,
            relay_hash,
            relay_data.clone(),
        );
        let finalize_ix = self.finalize_ix(relay_hash, &verified_fill);

        let lookup_addresses =
            representative_lookup_addresses(&[verify_ix.clone(), finalize_ix.clone()]);
        let lookup_table = prepare_lookup_table(&self.rpc, &self.payer, lookup_addresses)?;

        send_versioned_transaction(
            &self.rpc,
            "verify_and_finalize",
            &self.payer,
            &[],
            &[compute_budget_ix(), verify_ix, finalize_ix],
            &[lookup_table],
        )?;
        Ok(relay_hash)
    }

    fn finalize_ix(&self, fill_id: [u8; 32], verified_fill: &Pubkey) -> Instruction {
        finalize_fill_ix(
            &self.payer.pubkey(),
            &self.route,
            self.route_config_hash,
            verified_fill,
            fill_id,
            &USDC_MINT,
        )
    }

    /// Plain SPL transfer of unrelated funds into the route vault — the
    /// "another fill is already escrowed / someone topped the vault up" state
    /// finalize must NOT sweep.
    fn seed_route_vault_dust(&self) -> TestResult<()> {
        let transfer_ix = anchor_spl::token::spl_token::instruction::transfer(
            &anchor_spl::token::ID,
            &self.relayer_ata,
            &self.route_vault,
            &self.relayer.pubkey(),
            &[],
            ROUTE_VAULT_DUST,
        )?;
        send_legacy_transaction(
            &self.rpc,
            "seed_route_vault_dust",
            &self.relayer,
            &[],
            &[transfer_ix],
        )?;
        Ok(())
    }

    /// `execute_actions` (payer is the allowed executor) as a v0 transaction
    /// with a real address lookup table — the shape the SDK submits.
    fn execute_actions(&self, fill_id: [u8; 32]) -> TestResult<()> {
        let execute_ix = execute_actions_ix(
            &self.payer.pubkey(),
            &self.route,
            fill_id,
            &USDC_MINT,
            &[self.recipient_a_ata, self.recipient_b_ata],
        );
        let lookup_addresses = representative_lookup_addresses(std::slice::from_ref(&execute_ix));
        let lookup_table = prepare_lookup_table(&self.rpc, &self.payer, lookup_addresses)?;
        send_versioned_transaction(
            &self.rpc,
            "execute_actions",
            &self.payer,
            &[],
            &[compute_budget_ix(), execute_ix],
            &[lookup_table],
        )?;
        Ok(())
    }

    fn recover_funds(&self, fill_id: [u8; 32]) -> TestResult<()> {
        let recover_ix = recover_funds_ix(
            &self.owner.pubkey(),
            &self.route,
            &fill_id,
            &USDC_MINT,
            &self.owner_ata,
        );
        send_legacy_transaction(
            &self.rpc,
            "recover_funds",
            &self.payer,
            &[&self.owner],
            &[recover_ix],
        )?;
        Ok(())
    }

    fn token_amount(&self, ata: &Pubkey) -> TestResult<u64> {
        Ok(self.rpc.get_token_account_balance(ata)?.amount.parse()?)
    }

    fn fill_receipt(&self, fill_id: [u8; 32]) -> TestResult<FillReceipt> {
        let (fill_receipt, _) = derive_fill_receipt_pda(&self.route, &fill_id);
        read_anchor_account(&self.rpc, &fill_receipt)
    }
}

/// Count `Program <spoke> invoke/success` lines — the fill leg must show the
/// real program running (top-level fill_relay + its emit_cpi self-invoke).
fn assert_real_spoke_pool_ran(logs: &[String]) {
    let invoke_prefix = format!("Program {} invoke", ACROSS_SPOKE_POOL_ID);
    let success_prefix = format!("Program {} success", ACROSS_SPOKE_POOL_ID);
    let across_invokes = logs
        .iter()
        .filter(|line| line.contains(&invoke_prefix))
        .count();
    let across_successes = logs
        .iter()
        .filter(|line| line.contains(&success_prefix))
        .count();
    assert!(
        across_invokes >= 2,
        "expected ≥2 Across invocations (fill_relay + emit_cpi event), got {across_invokes}"
    );
    assert!(
        across_successes >= 2,
        "expected ≥2 Across success lines (fill_relay + emit_cpi event), got {across_successes}"
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn dest_e2e_real_across_happy_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolDestFixture::bootstrap().expect("bootstrap fixture");
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
    assert_real_spoke_pool_ran(&logs);
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
    let fx = SurfpoolDestFixture::bootstrap().expect("bootstrap fixture");
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
    let fx = SurfpoolDestFixture::bootstrap().expect("bootstrap fixture");
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
    let replay = send_legacy_transaction(
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
    assert!(
        replay_err.contains("already in use"),
        "replay must be rejected on the existing FillReceipt account (init collision), got: {replay_err}"
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
}
