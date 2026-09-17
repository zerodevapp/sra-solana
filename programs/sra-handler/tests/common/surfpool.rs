//! Shared Surfpool E2E harness.
//!
//! Both end-to-end suites (`source_e2e_surfpool.rs`, `dest_e2e_surfpool.rs`)
//! target the SAME Surfpool instance at `SURFPOOL_RPC_URL` — a mainnet fork
//! carrying the real Across SVM Spoke Pool, the real CCTP v2 programs, and
//! real USDC. Everything here is transport plumbing: readiness/deploy
//! polling, native-v1 transaction submission with inline addresses and
//! TransactionConfig resource limits, Surfpool cheat-code RPC, log retrieval,
//! and the CCTP fork setup (attester takeover + the keeper-reference
//! `receive_message` builder).
//!
//! Serialization: the suites live in separate test binaries, i.e. separate
//! processes, so an in-process mutex cannot serialize them. [`surfpool_lock`]
//! takes an OS-level advisory file lock instead; it also serializes tests
//! WITHIN a binary (each open handle contends), replacing the old file-local
//! `SURFPOOL_TEST_MUTEX`. The lock releases on drop, including on panic.

use anchor_lang::{AccountDeserialize, AccountSerialize, AnchorSerialize};
use fslock::LockFile;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::versioned::VersionedTransaction;
use sra_cctp_adapter::utils::cctp_state::{
    deserialize_message_transmitter, MESSAGE_TRANSMITTER_DISCRIMINATOR,
};
use sra_common::{
    constants::{CCTP_MESSAGE_TRANSMITTER_V2_ID, CCTP_TOKEN_MESSENGER_MINTER_V2_ID, USDC_MINT},
    instruction_data::encode_anchor_instruction_data,
    types::FeeConfig,
};
use std::{env, error::Error, path::PathBuf, thread::sleep, time::Duration};

use super::cctp::{derive_transmitter_state_pda, derive_used_nonce_pda};
use super::v1::{build_v1_transaction, DEFAULT_V1_CONFIG};
use super::{
    derive_ata, derive_fee_config_pda, initialize_fee_config_ix, set_fee_recipient_ix,
    set_protocol_fee_ix,
};

pub const SURFPOOL_RPC_URL: &str = "http://127.0.0.1:8899";
pub const SURFPOOL_RETRY_COUNT: usize = 30;
pub const SURFPOOL_RETRY_DELAY_MS: u64 = 1_000;
pub const SURFPOOL_HTTP_TIMEOUT_SECS: u64 = 30;

pub type TestResult<T> = Result<T, Box<dyn Error>>;

pub fn surfpool_log(message: impl AsRef<str>) {
    eprintln!("[surfpool-test] {}", message.as_ref());
}

/// Serialize all Surfpool E2E tests — across processes AND within one binary —
/// via an OS advisory lock on a well-known temp file. Hold the returned handle
/// for the duration of the test.
pub fn surfpool_lock() -> TestResult<LockFile> {
    let path = env::temp_dir().join("sra-solana-surfpool-e2e.lock");
    let mut lock = LockFile::open(&path)?;
    surfpool_log(format!("surfpool_lock: waiting for {} ...", path.display()));
    lock.lock()?;
    surfpool_log("surfpool_lock: acquired");
    Ok(lock)
}

pub fn payer_keypair_path() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME must be set")).join(".config/solana/id.json")
}

pub fn rpc_client() -> RpcClient {
    RpcClient::new_with_commitment(SURFPOOL_RPC_URL.to_string(), CommitmentConfig::confirmed())
}

pub fn http_client() -> TestResult<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(SURFPOOL_HTTP_TIMEOUT_SECS))
        .build()?)
}

pub fn wait_for_surfpool(rpc: &RpcClient) -> TestResult<()> {
    for attempt in 1..=SURFPOOL_RETRY_COUNT {
        surfpool_log(format!(
            "wait_for_surfpool: attempt {attempt}/{SURFPOOL_RETRY_COUNT} against {SURFPOOL_RPC_URL}"
        ));
        match rpc.get_latest_blockhash() {
            Ok(_) => {
                surfpool_log(format!(
                    "wait_for_surfpool: RPC ready on attempt {attempt}/{SURFPOOL_RETRY_COUNT}"
                ));
                return Ok(());
            }
            Err(err) => {
                surfpool_log(format!(
                    "wait_for_surfpool: RPC not ready on attempt {attempt}/{SURFPOOL_RETRY_COUNT}: {err}"
                ));
            }
        }
        sleep(Duration::from_millis(SURFPOOL_RETRY_DELAY_MS));
    }

    surfpool_log(format!(
        "wait_for_surfpool: timed out after {SURFPOOL_RETRY_COUNT} attempts"
    ));
    Err("Surfpool RPC did not become ready".into())
}

pub fn wait_for_program_deploys(rpc: &RpcClient, program_ids: &[Pubkey]) -> TestResult<()> {
    let expected_programs = program_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    for attempt in 1..=SURFPOOL_RETRY_COUNT {
        surfpool_log(format!(
            "wait_for_program_deploys: attempt {attempt}/{SURFPOOL_RETRY_COUNT} for [{expected_programs}]"
        ));
        if program_ids
            .iter()
            .all(|program_id| rpc.get_account(program_id).is_ok())
        {
            surfpool_log(format!(
                "wait_for_program_deploys: all programs detected on attempt {attempt}/{SURFPOOL_RETRY_COUNT}"
            ));
            return Ok(());
        }
        sleep(Duration::from_millis(SURFPOOL_RETRY_DELAY_MS));
    }

    surfpool_log(format!(
        "wait_for_program_deploys: timed out after {SURFPOOL_RETRY_COUNT} attempts for [{expected_programs}]"
    ));
    Err("Surfpool did not finish auto-deploying local programs".into())
}

/// Request an airdrop and poll until it CONFIRMS. `confirm_transaction`
/// returns `Ok(false)` while the transaction is still unconfirmed — treating
/// that as success would let later steps fail in harder-to-debug ways.
pub fn request_and_confirm_airdrop(
    rpc: &RpcClient,
    label: &str,
    key: &Pubkey,
    lamports: u64,
) -> TestResult<Signature> {
    let signature = rpc.request_airdrop(key, lamports)?;
    for attempt in 1..=SURFPOOL_RETRY_COUNT {
        if rpc.confirm_transaction(&signature)? {
            surfpool_log(format!(
                "request_and_confirm_airdrop[{label}]: confirmed signature {signature} on attempt {attempt}/{SURFPOOL_RETRY_COUNT}"
            ));
            return Ok(signature);
        }
        surfpool_log(format!(
            "request_and_confirm_airdrop[{label}]: not yet confirmed on attempt {attempt}/{SURFPOOL_RETRY_COUNT}"
        ));
        sleep(Duration::from_millis(SURFPOOL_RETRY_DELAY_MS));
    }
    Err(format!("airdrop for {label} ({key}) did not confirm: {signature}").into())
}

pub fn build_surfpool_v1_transaction(
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
    blockhash: Hash,
) -> TestResult<VersionedTransaction> {
    build_v1_transaction(payer, signers, instructions, blockhash, DEFAULT_V1_CONFIG)
}

pub fn send_v1_transaction(
    rpc: &RpcClient,
    label: &str,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
) -> TestResult<Signature> {
    let transaction =
        build_surfpool_v1_transaction(payer, signers, instructions, rpc.get_latest_blockhash()?)?;
    let signature = rpc.send_and_confirm_transaction(&transaction)?;
    surfpool_log(format!(
        "send_v1_transaction[{label}]: confirmed signature {signature}"
    ));
    Ok(signature)
}

/// Call a Surfpool cheat-code / extension RPC method (e.g.
/// `surfnet_setTokenAccount`, `surfnet_getLocalSignatures`).
pub fn surfpool_rpc(http: &Client, method: &str, params: Value) -> TestResult<Value> {
    surfpool_log(format!("surfpool_rpc[{method}]: sending request"));
    let response: Value = http
        .post(SURFPOOL_RPC_URL)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()?
        .error_for_status()?
        .json()?;

    if let Some(err) = response.get("error") {
        surfpool_log(format!("surfpool_rpc[{method}]: error {err}"));
        return Err(format!("{method} failed: {err}").into());
    }

    surfpool_log(format!("surfpool_rpc[{method}]: request succeeded"));
    Ok(response["result"].clone())
}

pub fn surfpool_set_account_data(http: &Client, address: &Pubkey, data: &[u8]) -> TestResult<()> {
    let hex_data: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
    surfpool_rpc(
        http,
        "surfnet_setAccount",
        json!([address.to_string(), { "data": hex_data }]),
    )?;
    Ok(())
}

pub fn configure_protocol_fee(
    rpc: &RpcClient,
    http: &Client,
    payer: &Keypair,
    fee_bps: u16,
) -> TestResult<Option<Pubkey>> {
    let fee_config = derive_fee_config_pda().0;
    if rpc.get_account(&fee_config).is_ok() {
        let mut config: FeeConfig = read_anchor_account(rpc, &fee_config)?;
        config.admin = payer.pubkey();
        let mut data = Vec::new();
        config.try_serialize(&mut data)?;
        surfpool_set_account_data(http, &fee_config, &data)?;
    } else {
        send_v1_transaction(
            rpc,
            "initialize_fee_config",
            payer,
            &[],
            &[initialize_fee_config_ix(
                &payer.pubkey(),
                &payer.pubkey(),
                Pubkey::default(),
            )],
        )?;
    }
    let fee_recipient = if fee_bps == 0 {
        Pubkey::default()
    } else {
        Pubkey::new_unique()
    };
    send_v1_transaction(
        rpc,
        "configure_protocol_fee",
        payer,
        &[],
        &[
            set_fee_recipient_ix(&payer.pubkey(), fee_recipient),
            set_protocol_fee_ix(&payer.pubkey(), &payer.pubkey(), fee_bps, false, false),
        ],
    )?;
    if fee_bps == 0 {
        return Ok(None);
    }
    surfpool_rpc(
        http,
        "surfnet_setTokenAccount",
        json!([fee_recipient.to_string(), USDC_MINT.to_string(), { "amount": 0 }, anchor_spl::token::ID.to_string()]),
    )?;
    Ok(Some(derive_ata(&fee_recipient, &USDC_MINT)))
}

/// Across paused deposits AND fills on the live Solana spoke (observed
/// 2026-07-23), and the fork inherits that state, failing every deposit/fill
/// with `DepositsArePaused`/`FillsArePaused`. Clear the two pause bools —
/// the first two bytes after the 8-byte anchor discriminator in the spoke's
/// `State` account — so the suites keep exercising the unpaused paths they
/// encode. Idempotent; a no-op once Across unpauses.
pub fn unpause_across_spoke_state(
    rpc: &RpcClient,
    http: &Client,
    state: &Pubkey,
) -> TestResult<()> {
    let mut data = rpc.get_account_data(state)?;
    if data.len() < 10 {
        return Err(format!("spoke state {state} too short: {} bytes", data.len()).into());
    }
    if data[8] == 0 && data[9] == 0 {
        surfpool_log("unpause_across_spoke_state: spoke already unpaused");
        return Ok(());
    }
    data[8] = 0; // paused_deposits
    data[9] = 0; // paused_fills
    surfpool_set_account_data(http, state, &data)?;
    surfpool_log("unpause_across_spoke_state: cleared pause flags on the fork");
    Ok(())
}

/// Fetch the execution logs of a locally processed signature via Surfpool's
/// `surfnet_getLocalSignatures` extension.
pub fn logs_for_signature(http: &Client, signature: &Signature) -> TestResult<Vec<String>> {
    let local = surfpool_rpc(http, "surfnet_getLocalSignatures", json!([20]))?;
    let entries = local["value"]
        .as_array()
        .ok_or("surfnet_getLocalSignatures returned non-array value")?;
    let entry = entries
        .iter()
        .find(|entry| entry["signature"].as_str() == Some(&signature.to_string()))
        .ok_or("signature not found in Surfpool local signatures")?;
    let logs = entry["logs"]
        .as_array()
        .ok_or("signature entry missing logs")?
        .iter()
        .map(|line| {
            line.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "log line was not a string".into())
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(logs)
}

/// The REAL bridge program (not a mock) must appear in the execution logs at
/// least `min_invocations` times — as both `Program <id> invoke` and
/// `Program <id> success` lines (the top-level CPI plus any nested
/// self-invokes, e.g. `emit_cpi`).
pub fn assert_program_ran(
    logs: &[String],
    label: &str,
    program_id: &Pubkey,
    min_invocations: usize,
) {
    let invoke_prefix = format!("Program {program_id} invoke");
    let success_prefix = format!("Program {program_id} success");
    let invokes = logs
        .iter()
        .filter(|line| line.contains(&invoke_prefix))
        .count();
    let successes = logs
        .iter()
        .filter(|line| line.contains(&success_prefix))
        .count();
    assert!(
        invokes >= min_invocations && successes >= min_invocations,
        "expected the real {label} program {program_id} to run ≥{min_invocations}×, \
         got {invokes} invoke(s) / {successes} success(es)"
    );
}

pub fn read_anchor_account<T: AccountDeserialize>(
    rpc: &RpcClient,
    address: &Pubkey,
) -> TestResult<T> {
    let data = rpc.get_account_data(address)?;
    Ok(T::try_deserialize(&mut data.as_slice())?)
}

/// The fork's current unix time, from the on-chain `Clock` sysvar — the time
/// base the real Across program checks fill deadlines against.
pub fn clock_unix_timestamp(rpc: &RpcClient) -> TestResult<i64> {
    let data = rpc.get_account_data(&solana_program::sysvar::clock::ID)?;
    let clock: Clock = bincode::deserialize(&data)?;
    Ok(clock.unix_timestamp)
}

// ---------------------------------------------------------------------------
// CCTP v2 fork setup + keeper-reference builders
// ---------------------------------------------------------------------------

/// Live CCTP v2 mainnet values, pinned once from the real accounts via
/// `get_account` + the exact layouts (read 2026-07-30):
/// - MessageTransmitter `W1k5ijkaSTo5iA5zChNpfzcy796fLhkBxfmJuR8W8HU`:
///   `version = 1` (pinned as `common::cctp::CCTP_HEADER_VERSION`),
///   `signature_threshold = 2` over 2 enabled attesters (both replaced on the
///   fork by [`override_transmitter_attesters`]).
/// - TokenMessenger `AawthJCGRmggpfv9MMWV6Jmo9cue4gL9wUZgRBShg58W`:
///   `message_body_version = 1` (pinned as the adapter's
///   `BURN_MESSAGE_BODY_VERSION`), `min_fee = 0` (so `max_fee = 0` deposits
///   clear the on-chain fee floor), and `fee_recipient` below (its USDC ATA
///   collects `fee_executed` on every mint).
pub const CCTP_FEE_RECIPIENT: Pubkey =
    Pubkey::from_str_const("4BPnUzFDibVcWQ5zzixGodRUHwqDxHYpUPdPYus3Bn56");

/// Circle's fee-collection USDC account on the mint leg: the ATA of the live
/// `token_messenger.fee_recipient`.
pub fn cctp_fee_recipient_ata() -> Pubkey {
    derive_ata(&CCTP_FEE_RECIPIENT, &USDC_MINT)
}

/// Take over the fork's CCTP v2 attester set: re-encode the REAL transmitter
/// state with `signature_threshold = 1` and `enabled_attesters = [attester]`
/// — a full data rewrite via `surfnet_setAccount`, the same pattern as
/// [`unpause_across_spoke_state`]. The one overridden account then feeds both
/// the real `receive_message` and the adapter's `verify_cctp_mint`, keeping
/// the fork self-consistent. Idempotent.
pub fn override_transmitter_attesters(
    rpc: &RpcClient,
    http: &Client,
    attester: Pubkey,
) -> TestResult<()> {
    let state_pda = derive_transmitter_state_pda();
    let mut state = deserialize_message_transmitter(&rpc.get_account_data(&state_pda)?)
        .map_err(|err| format!("transmitter state {state_pda} failed to parse: {err}"))?;
    if state.signature_threshold == 1 && state.enabled_attesters == [attester] {
        surfpool_log("override_transmitter_attesters: fork already carries the test attester");
        return Ok(());
    }
    state.signature_threshold = 1;
    state.enabled_attesters = vec![attester];
    let mut data = MESSAGE_TRANSMITTER_DISCRIMINATOR.to_vec();
    state.serialize(&mut data)?;
    surfpool_set_account_data(http, &state_pda, &data)?;
    surfpool_log("override_transmitter_attesters: installed the test attester (threshold 1)");
    Ok(())
}

/// The EVM TokenMessenger the fork's CCTP config trusts for `remote_domain` —
/// the 32-byte `sender` a crafted burn message must carry. Read live from the
/// `RemoteTokenMessenger` PDA (layout: 8-byte discriminator ‖ `domain: u32` ‖
/// `token_messenger: Pubkey`).
pub fn cctp_remote_token_messenger(rpc: &RpcClient, remote_domain: u32) -> TestResult<[u8; 32]> {
    let pda = Pubkey::find_program_address(
        &[
            b"remote_token_messenger",
            remote_domain.to_string().as_bytes(),
        ],
        &CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
    )
    .0;
    let data = rpc.get_account_data(&pda)?;
    let discriminator = solana_program::hash::hash(b"account:RemoteTokenMessenger").to_bytes();
    if data.len() != 44
        || data[..8] != discriminator[..8]
        || u32::from_le_bytes(data[8..12].try_into()?) != remote_domain
    {
        return Err(format!("remote token messenger {pda} has an unexpected layout").into());
    }
    Ok(data[12..44].try_into()?)
}

/// Anchor wire params of the transmitter's `receive_message`.
#[derive(AnchorSerialize)]
struct ReceiveMessageParams {
    message: Vec<u8>,
    attestation: Vec<u8>,
}

/// The real MessageTransmitter v2 `receive_message` instruction for a USDC
/// burn message — the KEEPER REFERENCE account list. Fixed transmitter
/// accounts first (`payer` is also the permissionless `caller`), then the
/// `handle_receive_finalized_message` remaining accounts exactly as Circle's
/// client encodes them. The `remote_token_messenger` / `token_pair` seeds
/// encode `remote_domain` as DECIMAL-STRING bytes; `remote_usdc` is the
/// source chain's USDC address, zero-padded to 32 bytes.
pub fn receive_message_ix(
    payer: &Pubkey,
    nonce: [u8; 32],
    message: Vec<u8>,
    attestation: Vec<u8>,
    remote_domain: u32,
    remote_usdc: [u8; 32],
    recipient_token_account: &Pubkey,
) -> Instruction {
    let tmm = CCTP_TOKEN_MESSENGER_MINTER_V2_ID;
    let transmitter = CCTP_MESSAGE_TRANSMITTER_V2_ID;
    let domain_seed = remote_domain.to_string();
    let find = |seeds: &[&[u8]], program: &Pubkey| Pubkey::find_program_address(seeds, program).0;
    Instruction {
        program_id: transmitter,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*payer, true), // caller (permissionless receive)
            AccountMeta::new_readonly(
                find(
                    &[b"message_transmitter_authority", tmm.as_ref()],
                    &transmitter,
                ),
                false,
            ),
            AccountMeta::new_readonly(derive_transmitter_state_pda(), false),
            AccountMeta::new(derive_used_nonce_pda(&nonce), false),
            AccountMeta::new_readonly(tmm, false), // receiver program
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(find(&[b"__event_authority"], &transmitter), false),
            AccountMeta::new_readonly(transmitter, false),
            // remaining accounts → handle_receive_finalized_message
            AccountMeta::new_readonly(find(&[b"token_messenger"], &tmm), false),
            AccountMeta::new_readonly(
                find(&[b"remote_token_messenger", domain_seed.as_bytes()], &tmm),
                false,
            ),
            AccountMeta::new(find(&[b"token_minter"], &tmm), false),
            AccountMeta::new(find(&[b"local_token", USDC_MINT.as_ref()], &tmm), false),
            AccountMeta::new_readonly(
                find(
                    &[b"token_pair", domain_seed.as_bytes(), remote_usdc.as_ref()],
                    &tmm,
                ),
                false,
            ),
            AccountMeta::new(cctp_fee_recipient_ata(), false),
            AccountMeta::new(*recipient_token_account, false),
            AccountMeta::new(find(&[b"custody", USDC_MINT.as_ref()], &tmm), false),
            AccountMeta::new_readonly(anchor_spl::token::ID, false),
            AccountMeta::new_readonly(find(&[b"__event_authority"], &tmm), false),
            AccountMeta::new_readonly(tmm, false),
        ],
        data: encode_anchor_instruction_data(
            "receive_message",
            &ReceiveMessageParams {
                message,
                attestation,
            },
        ),
    }
}
