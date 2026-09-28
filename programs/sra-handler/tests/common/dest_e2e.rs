//! The destination-E2E fixture: one fresh route driven end-to-end against
//! whichever REAL bridge program the route allowlists — the Across
//! `fill_relay` or the CCTP v2 `receive_message` — plus the settle leg the
//! two adapters share and the log assertions that prove the genuine
//! programs ran. The test binary holds only scenarios; every account,
//! amount, and wire-crafting decision lives here. Repository-constructed
//! application transactions use native v1 with inline addresses and explicit
//! `TransactionConfig` resource limits.

use anchor_spl::associated_token::spl_associated_token_account;
use hex_literal::hex;
use libsecp256k1::SecretKey;
use reqwest::blocking::Client;
use serde_json::json;
use solana_instruction::Instruction;
use solana_keypair::{read_keypair_file, Keypair};
use solana_program::keccak;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use sra_across_adapter::utils::svm_spoke;
use sra_across_adapter::utils::svm_spoke::types::RelayData;
use sra_common::{
    abi::u256_from_u64,
    constants::{ACROSS_SPOKE_POOL_ID, SOLANA_CHAIN_ID, USDC_MINT},
    types::{CreateRouteParams, FillReceipt},
};

use super::cctp::{
    derive_cctp_verified_fill_pda, derive_used_nonce_pda, sample_cctp_message, sign_attestation,
    test_attester, verify_cctp_mint_ix, ARBITRUM_CCTP_DOMAIN,
};
use super::surfpool::*;
use super::*;

/// One USDC (6 decimals) — the amount the relayer delivers to the route.
pub const OUTPUT_AMOUNT: u64 = 1_000_000;
pub const RELAYER_STARTING_BALANCE: u64 = 10_000_000;
/// Matches the route's committed `source_chain_id` (Arbitrum One).
pub const ORIGIN_CHAIN_ID: u64 = 42_161;
/// How far past the fork's current time the fill deadline sits.
pub const FILL_DEADLINE_WINDOW_SECS: i64 = 3_600;
/// The route commits a 60/40 SplTransfer split. Distinct per-recipient
/// amounts keep "pay bps of the snapshot" distinguishable from "pay the whole
/// vault" (with a single 100% action every correct and incorrect amount
/// formula produces the same number).
pub const SPLIT_A_BPS: u16 = 6_000;
pub const SPLIT_B_BPS: u16 = 4_000;
pub const SPLIT_A_AMOUNT: u64 = 600_000;
pub const SPLIT_B_AMOUNT: u64 = 400_000;
/// Unrelated balance seeded into the route vault before finalization. The
/// vault must hold exactly this after `finalize_fill` — pinning that finalize
/// moves exactly the verified fill amount, not the vault's whole balance.
pub const ROUTE_VAULT_DUST: u64 = 12_345;
/// The replay test's second, larger fill: ≥ the first fill's amount so a
/// hypothetical replay double-draw would actually have funds to steal, and
/// distinct from it so amounts never alias.
pub const SECOND_FILL_AMOUNT: u64 = 1_500_000;
/// The CCTP fast-transfer fee Iris would have taken, for the
/// `fee_executed > 0` run: delivered = amount − fee, and the fee lands in
/// Circle's fee-recipient ATA. Chosen so the delivered 60/40 split still
/// produces exact per-recipient amounts (997_500 → 598_500 / 399_000).
pub const CCTP_FEE_EXECUTED: u64 = 2_500;
/// Headroom the source-side quote committed to: `fee_executed ≤ max_fee`.
pub const CCTP_MAX_FEE: u64 = 5_000;
/// Arbitrum's native USDC (`0xaf88d065e77c8cC2239327C5EDb3A432268e5831`),
/// zero-padded to the 32-byte wire form — the crafted burn message must carry
/// the real remote token so the fork's genuine `token_pair` account
/// (domain 3) resolves it on `receive_message`.
pub const ARBITRUM_USDC: [u8; 32] =
    hex!("000000000000000000000000af88d065e77c8cc2239327c5edb3a432268e5831");

/// The delegate signer the real `fill_relay` moves relayer funds through:
/// `["delegate", keccak256(borsh(relay_hash ‖ repayment_chain_id ‖
/// repayment_address))]` under the spoke program. The relayer must approve it
/// for `output_amount` in the same transaction.
pub fn fill_delegate_pda(
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

/// A crafted CCTP burn message with its threshold-1 attestation — the wire
/// pair that feeds both the real `receive_message` and `verify_cctp_mint`.
pub struct AttestedMessage {
    pub nonce: [u8; 32],
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
}

pub struct SurfpoolDestFixture {
    pub rpc: RpcClient,
    pub http: Client,
    pub payer: Keypair,
    pub owner: Keypair,
    pub relayer: Keypair,
    /// The secp256k1 key the fork's overridden attester set trusts — crafted
    /// CCTP attestations are signed with it.
    pub attester: SecretKey,
    pub route: Pubkey,
    pub route_config_hash: [u8; 32],
    pub route_vault: Pubkey,
    pub execution_vault: Pubkey,
    pub relayer_ata: Pubkey,
    pub owner_ata: Pubkey,
    pub recipient_a_ata: Pubkey,
    pub recipient_b_ata: Pubkey,
}

impl SurfpoolDestFixture {
    /// Bootstrap a fresh route whose `allowed_adapters` is exactly
    /// `[adapter_program]` — the same fork prep runs for either adapter (the
    /// Across unpause and the CCTP attester takeover are both idempotent and
    /// keep the one shared fork consistent for both suites).
    pub fn bootstrap(adapter_program: Pubkey) -> TestResult<Self> {
        surfpool_log("bootstrap: starting Surfpool destination fixture setup");
        let rpc = rpc_client();
        wait_for_surfpool(&rpc)?;
        surfpool_log("bootstrap: Surfpool RPC is ready");

        let http = http_client()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let relayer = Keypair::new();
        let (attester, attester_address) = test_attester();
        let recipient_a = Keypair::new().pubkey();
        let recipient_b = Keypair::new().pubkey();

        wait_for_program_deploys(&rpc, &[PROGRAM_ID, adapter_program])?;
        surfpool_log("bootstrap: local programs are deployed");
        rpc.get_account(&ACROSS_SPOKE_POOL_ID)?;
        surfpool_log(format!(
            "bootstrap: Across spoke pool {ACROSS_SPOKE_POOL_ID} is reachable"
        ));
        unpause_across_spoke_state(&rpc, &http, &derive_real_across_state_pda().0)?;
        override_transmitter_attesters(&rpc, &http, attester_address)?;

        for (label, key, lamports) in [
            ("payer", payer.pubkey(), 10 * SOL),
            ("owner", owner.pubkey(), SOL),
            ("relayer", relayer.pubkey(), SOL),
        ] {
            request_and_confirm_airdrop(&rpc, label, &key, lamports)?;
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
            allowed_adapters: vec![adapter_program],
            allowed_executors: vec![payer.pubkey()],
        };
        let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
        let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &salt);
        let create_route = create_route_ix_with_hash(
            &payer.pubkey(),
            &owner.pubkey(),
            route_config_hash,
            params,
            &[adapter_program],
        );
        send_v1_transaction(&rpc, "create_route", &payer, &[&owner], &[create_route])?;
        surfpool_log(format!("bootstrap: created route {route}"));

        let (execution_authority, _) = derive_execution_authority_pda(&route);
        let route_vault = derive_ata(&route, &USDC_MINT);
        let execution_vault = derive_ata(&execution_authority, &USDC_MINT);
        let relayer_ata = derive_ata(&relayer.pubkey(), &USDC_MINT);
        let owner_ata = derive_ata(&owner.pubkey(), &USDC_MINT);
        let recipient_a_ata = derive_ata(&recipient_a, &USDC_MINT);
        let recipient_b_ata = derive_ata(&recipient_b, &USDC_MINT);

        // ATAs the flow pays into (and the relayer's funding ATA). The route
        // and execution vaults are created by `create_route` itself; Circle's
        // fee-recipient ATA collects `fee_executed` on CCTP mints.
        let create_atas: Vec<Instruction> = [
            (&relayer.pubkey(), "relayer"),
            (&owner.pubkey(), "owner"),
            (&recipient_a, "action recipient A"),
            (&recipient_b, "action recipient B"),
            (&CCTP_FEE_RECIPIENT, "CCTP fee recipient"),
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
        send_v1_transaction(&rpc, "create_atas", &payer, &[], &create_atas)?;
        surfpool_log("bootstrap: ensured relayer/owner/recipient/fee-recipient ATAs");

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
            attester,
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
    pub fn relay_data(&self) -> TestResult<RelayData> {
        self.relay_data_with(self.route.to_bytes(), OUTPUT_AMOUNT)
    }

    /// [`Self::relay_data`] with a caller-chosen `deposit_id` and amount — a
    /// SECOND, distinct deposit against the same route (the replay test fills
    /// two deposits so the route vault is funded when the first fill's
    /// settlement is replayed).
    pub fn relay_data_with(
        &self,
        deposit_id: [u8; 32],
        output_amount: u64,
    ) -> TestResult<RelayData> {
        let now = clock_unix_timestamp(&self.rpc)?;
        let fill_deadline = u32::try_from(now + FILL_DEADLINE_WINDOW_SECS)
            .map_err(|_| "fill deadline exceeds u32")?;
        Ok(RelayData {
            depositor: Pubkey::new_from_array([0x11; 32]),
            recipient: self.route,
            exclusive_relayer: self.relayer.pubkey(),
            input_token: Pubkey::new_from_array([0x33; 32]),
            output_token: USDC_MINT,
            input_amount: u256_from_u64(output_amount),
            output_amount,
            origin_chain_id: ORIGIN_CHAIN_ID,
            deposit_id,
            fill_deadline,
            exclusivity_deadline: fill_deadline,
            message: vec![],
        })
    }

    /// The relayer executes the REAL `fill_relay` on the fork: approves the
    /// fill delegate for `output_amount`, then fills. The real program
    /// validates the relay hash against its own preimage, moves
    /// `output_amount` from the relayer's ATA to the route vault, and creates
    /// the genuine `FillStatusAccount` at `["fills", relay_hash]`.
    pub fn execute_real_fill(
        &self,
        relay_data: &RelayData,
    ) -> TestResult<(Signature, Vec<String>)> {
        let relay_hash = relay_hash_for(relay_data);
        let repayment_chain_id = SOLANA_CHAIN_ID;
        let repayment_address = self.relayer.pubkey();
        let delegate = fill_delegate_pda(relay_hash, repayment_chain_id, repayment_address);
        let (fill_status, _) = derive_fill_status_pda(&relay_hash);
        surfpool_log(format!(
            "execute_real_fill: relay_hash={} fill_status={fill_status} delegate={delegate}",
            bs58::encode(relay_hash).into_string()
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
                associated_token_program: spl_associated_token_account::program::ID,
                system_program: solana_system_interface::program::ID,
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

        let signature = send_v1_transaction(
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

    /// The settle leg both adapters share: `verify_*` + `finalize_fill` as one
    /// native-v1 transaction with inline addresses and a 400k CU limit.
    pub fn submit_settlement(
        &self,
        verify_ix: Instruction,
        finalize_ix: Instruction,
    ) -> TestResult<()> {
        send_v1_transaction(
            &self.rpc,
            "verify_and_finalize",
            &self.payer,
            &[],
            &[verify_ix, finalize_ix],
        )?;
        Ok(())
    }

    pub fn verify_and_finalize(&self, relay_data: &RelayData) -> TestResult<[u8; 32]> {
        let relay_hash = relay_hash_for(relay_data);
        let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
        let verify_ix = verify_across_fill_ix(
            &self.payer.pubkey(),
            &self.route,
            relay_hash,
            relay_data.clone(),
        );
        self.submit_settlement(verify_ix, self.finalize_ix(relay_hash, &verified_fill))?;
        Ok(relay_hash)
    }

    /// A burn message for this fixture's route plus a threshold-1 attestation
    /// the fork's overridden attester set accepts. The nonce is
    /// `keccak(route ‖ counter)` — the fresh route makes it unique on the
    /// shared fork, the counter de-aliases messages within one test. The
    /// `fee_executed`/`expiration_block` slots are Iris-set fields the test
    /// writes directly; `sender`/`burn_token` carry the REAL domain-3 values
    /// so the fork's genuine trust config accepts the message.
    pub fn craft_attested_message(
        &self,
        counter: u64,
        amount: u64,
        max_fee: u64,
        fee_executed: u64,
    ) -> TestResult<AttestedMessage> {
        let preimage = [self.route.as_ref(), counter.to_le_bytes().as_ref()].concat();
        let nonce = keccak::hash(&preimage).to_bytes();
        let mut message = sample_cctp_message(
            nonce,
            &self.route,
            &USDC_MINT,
            amount,
            max_fee,
            fee_executed,
        );
        message.sender = cctp_remote_token_messenger(&self.rpc, ARBITRUM_CCTP_DOMAIN)?;
        message.burn_token = ARBITRUM_USDC;
        let message = message.encode();
        let attestation = sign_attestation(&message, &[&self.attester]);
        Ok(AttestedMessage {
            nonce,
            message,
            attestation,
        })
    }

    /// The REAL `receive_message` on the fork: the transmitter verifies the
    /// attestation against the overridden attester set, creates the genuine
    /// `used_nonce` account, and CPIs the TokenMessengerMinter, which pays
    /// `amount − fee_executed` from custody into the route vault and
    /// `fee_executed` into Circle's fee-recipient ATA.
    pub fn execute_real_receive(&self, msg: &AttestedMessage) -> TestResult<Vec<String>> {
        surfpool_log(format!(
            "execute_real_receive: nonce={} used_nonce={}",
            bs58::encode(msg.nonce).into_string(),
            derive_used_nonce_pda(&msg.nonce)
        ));
        let ix = receive_message_ix(
            &self.payer.pubkey(),
            msg.nonce,
            msg.message.clone(),
            msg.attestation.clone(),
            ARBITRUM_CCTP_DOMAIN,
            ARBITRUM_USDC,
            &self.route_vault,
        );
        let signature = send_v1_transaction(&self.rpc, "receive_message", &self.payer, &[], &[ix])?;
        logs_for_signature(&self.http, &signature)
    }

    /// `verify_cctp_mint` + `finalize_fill` (fill id = the message nonce) as
    /// the same one-transaction settle leg the Across path uses.
    pub fn verify_and_finalize_cctp(&self, msg: &AttestedMessage) -> TestResult<[u8; 32]> {
        let (verified_fill, _) = derive_cctp_verified_fill_pda(&msg.nonce);
        let verify_ix = verify_cctp_mint_ix(
            &self.payer.pubkey(),
            &self.route,
            msg.nonce,
            msg.message.clone(),
            msg.attestation.clone(),
        );
        self.submit_settlement(verify_ix, self.finalize_ix(msg.nonce, &verified_fill))?;
        Ok(msg.nonce)
    }

    pub fn finalize_ix(&self, fill_id: [u8; 32], verified_fill: &Pubkey) -> Instruction {
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
    pub fn seed_route_vault_dust(&self) -> TestResult<()> {
        let transfer_ix = anchor_spl::token::spl_token::instruction::transfer(
            &anchor_spl::token::ID,
            &self.relayer_ata,
            &self.route_vault,
            &self.relayer.pubkey(),
            &[],
            ROUTE_VAULT_DUST,
        )?;
        send_v1_transaction(
            &self.rpc,
            "seed_route_vault_dust",
            &self.relayer,
            &[],
            &[transfer_ix],
        )?;
        Ok(())
    }

    /// `execute_actions` (payer is the allowed executor) as a native-v1
    /// transaction with inline addresses.
    pub fn execute_actions(&self, fill_id: [u8; 32]) -> TestResult<()> {
        let execute_ix = execute_actions_ix(
            &self.payer.pubkey(),
            &self.route,
            fill_id,
            &USDC_MINT,
            &[self.recipient_a_ata, self.recipient_b_ata],
        );
        send_v1_transaction(
            &self.rpc,
            "execute_actions",
            &self.payer,
            &[],
            &[execute_ix],
        )?;
        Ok(())
    }

    pub fn recover_funds(&self, fill_id: [u8; 32]) -> TestResult<()> {
        let recover_ix = recover_funds_ix(
            &self.owner.pubkey(),
            &self.route,
            &fill_id,
            &USDC_MINT,
            &self.owner_ata,
        );
        send_v1_transaction(
            &self.rpc,
            "recover_funds",
            &self.payer,
            &[&self.owner],
            &[recover_ix],
        )?;
        Ok(())
    }

    pub fn token_amount(&self, ata: &Pubkey) -> TestResult<u64> {
        Ok(self.rpc.get_token_account_balance(ata)?.amount.parse()?)
    }

    pub fn fill_receipt(&self, fill_id: [u8; 32]) -> TestResult<FillReceipt> {
        let (fill_receipt, _) = derive_fill_receipt_pda(&self.route, &fill_id);
        read_anchor_account(&self.rpc, &fill_receipt)
    }
}
