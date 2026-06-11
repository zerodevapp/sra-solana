mod common;

use anchor_lang::AccountDeserialize;
use anchor_spl::associated_token::spl_associated_token_account;
use common::*;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use solana_client::rpc_client::RpcClient;
use solana_program::address_lookup_table::{
    instruction::{create_lookup_table, extend_lookup_table},
    state::AddressLookupTable,
    AddressLookupTableAccount,
};
use solana_sdk::{
    clock::Slot,
    commitment_config::CommitmentConfig,
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signature, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use sra_common::{
    constants::{ACROSS_SPOKE_POOL_ID, USDC_MINT},
    source_deposit::{AcrossDepositParams, PrepareParams},
    types::{MintNonce, SourceHandlerData, SraConfig},
};
use std::{env, error::Error, path::PathBuf, sync::Mutex, thread::sleep, time::Duration};

const INPUT_AMOUNT: u64 = 1_000_000;
// Quoted output must remain within `SLIPPAGE_BPS` of `INPUT_AMOUNT`, i.e.
// >= INPUT_AMOUNT * (BPS_DENOMINATOR - SLIPPAGE_BPS) / BPS_DENOMINATOR.
// 1_000_000 * 9_950 / 10_000 = 995_000, so 999_500 is comfortably within.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const TARGET_TOKEN_DECIMALS: u8 = 6;
// Across deposit window: 1h fill deadline, 15m exclusive-relayer window.
const FILL_DEADLINE_OFFSET_SECS: u32 = 3_600;
const EXCLUSIVITY_PERIOD_SECS: u32 = 900;
const SURFPOOL_RPC_URL: &str = "http://127.0.0.1:8899";
const SURFPOOL_RETRY_COUNT: usize = 30;
const SURFPOOL_RETRY_DELAY_MS: u64 = 1_000;
const SURFPOOL_HTTP_TIMEOUT_SECS: u64 = 30;

type TestResult<T> = Result<T, Box<dyn Error>>;

static SURFPOOL_TEST_MUTEX: Mutex<()> = Mutex::new(());

fn surfpool_log(message: impl AsRef<str>) {
    eprintln!("[surfpool-test] {}", message.as_ref());
}

fn default_prepare_params_rpc(_fx: &SurfpoolSourceFixture) -> PrepareParams {
    let mut params = sample_prepare_params();
    params.input_amount = INPUT_AMOUNT;
    params.output_amount = OUTPUT_WITHIN_SLIPPAGE;
    params
}

fn payer_keypair_path() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME must be set")).join(".config/solana/id.json")
}

fn wait_for_surfpool(rpc: &RpcClient) -> TestResult<()> {
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

fn wait_for_program_deploys(rpc: &RpcClient, program_ids: &[Pubkey]) -> TestResult<()> {
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

fn rpc_client() -> RpcClient {
    RpcClient::new_with_commitment(SURFPOOL_RPC_URL.to_string(), CommitmentConfig::confirmed())
}

fn send_legacy_transaction(
    rpc: &RpcClient,
    label: &str,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[solana_sdk::instruction::Instruction],
) -> TestResult<Signature> {
    let blockhash = rpc.get_latest_blockhash()?;
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all_signers, blockhash);
    surfpool_log(format!(
        "send_legacy_transaction[{label}]: sending {} instruction(s)",
        ixs.len()
    ));
    let signature = rpc.send_and_confirm_transaction(&tx)?;
    surfpool_log(format!(
        "send_legacy_transaction[{label}]: confirmed signature {signature}"
    ));
    Ok(signature)
}

fn send_versioned_transaction(
    rpc: &RpcClient,
    label: &str,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[solana_sdk::instruction::Instruction],
    lookup_tables: &[AddressLookupTableAccount],
) -> TestResult<Signature> {
    let blockhash = rpc.get_latest_blockhash()?;
    let message = v0::Message::try_compile(&payer.pubkey(), ixs, lookup_tables, blockhash)?;
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &all_signers)?;
    surfpool_log(format!(
        "send_versioned_transaction[{label}]: sending {} instruction(s) with {} lookup table(s)",
        ixs.len(),
        lookup_tables.len()
    ));
    let signature = rpc.send_and_confirm_transaction(&tx)?;
    surfpool_log(format!(
        "send_versioned_transaction[{label}]: confirmed signature {signature}"
    ));
    Ok(signature)
}

fn representative_lookup_table(ixs: &[solana_sdk::instruction::Instruction]) -> Vec<Pubkey> {
    let mut addresses = Vec::new();

    for ix in ixs {
        for meta in &ix.accounts {
            if meta.is_signer || addresses.contains(&meta.pubkey) {
                continue;
            }
            addresses.push(meta.pubkey);
        }
    }

    addresses
}

fn lookup_table_is_active(current_slot: Slot, last_extended_slot: Slot) -> bool {
    current_slot > last_extended_slot
}

fn lookup_table_account_from_rpc(
    rpc: &RpcClient,
    lookup_table_address: &Pubkey,
) -> TestResult<(AddressLookupTableAccount, Slot)> {
    let account = rpc.get_account(lookup_table_address)?;
    let table = AddressLookupTable::deserialize(account.data.as_slice())
        .map_err(|err| format!("failed to deserialize ALT: {err:?}"))?;
    Ok((
        AddressLookupTableAccount {
            key: *lookup_table_address,
            addresses: table.addresses.into_owned(),
        },
        table.meta.last_extended_slot,
    ))
}

#[test]
fn lookup_table_activation_waits_for_last_extended_slot() {
    assert!(!lookup_table_is_active(10, 10));
    assert!(!lookup_table_is_active(10, 11));
    assert!(lookup_table_is_active(12, 11));
}

fn read_anchor_account<T: AccountDeserialize>(rpc: &RpcClient, address: &Pubkey) -> TestResult<T> {
    let data = rpc.get_account_data(address)?;
    Ok(T::try_deserialize(&mut data.as_slice())?)
}

struct SurfpoolSourceFixture {
    rpc: RpcClient,
    http: Client,
    payer: Keypair,
    owner: Keypair,
    address_book_root: Pubkey,
    handler_data: SourceHandlerData,
    sra_config: Pubkey,
    target_token: [u8; 32],
    owner_ata: Pubkey,
    across_state: Pubkey,
    across_vault: Pubkey,
    accounts: ValidatePrepareAccounts,
}

impl SurfpoolSourceFixture {
    fn bootstrap() -> TestResult<Self> {
        surfpool_log("bootstrap: starting Surfpool source fixture setup");
        let rpc = rpc_client();
        wait_for_surfpool(&rpc)?;
        surfpool_log("bootstrap: Surfpool RPC is ready");

        let http = Client::builder()
            .timeout(Duration::from_secs(SURFPOOL_HTTP_TIMEOUT_SECS))
            .build()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let address_book_root = Keypair::new();
        let address_book_root_pubkey = address_book_root.pubkey();
        let handler_data = sample_handler_data(address_book_root_pubkey);
        let target_token = sample_target_token();
        let target_chain_id = handler_data.target_chain_id;
        let salt = [0xAB; 32];
        let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
        let accounts =
            ValidatePrepareAccounts::derive(&sra_config, &USDC_MINT, &ADAPTER_PROGRAM_ID);
        let owner_ata = derive_ata(&owner.pubkey(), &USDC_MINT);
        let (across_state, _) = derive_real_across_state_pda();
        let across_vault = derive_ata(&across_state, &USDC_MINT);

        wait_for_program_deploys(&rpc, &[PROGRAM_ID, ADAPTER_PROGRAM_ID])?;
        surfpool_log("bootstrap: local programs are deployed");
        rpc.get_account(&ACROSS_SPOKE_POOL_ID)?;
        surfpool_log(format!(
            "bootstrap: Across spoke pool {ACROSS_SPOKE_POOL_ID} is reachable"
        ));

        let payer_airdrop = rpc.request_airdrop(&payer.pubkey(), 10 * SOL)?;
        rpc.confirm_transaction(&payer_airdrop)?;
        surfpool_log(format!(
            "bootstrap: payer airdrop confirmed with signature {payer_airdrop}"
        ));

        let owner_airdrop = rpc.request_airdrop(&owner.pubkey(), SOL)?;
        rpc.confirm_transaction(&owner_airdrop)?;
        surfpool_log(format!(
            "bootstrap: owner airdrop confirmed with signature {owner_airdrop}"
        ));

        let initialize_address_book_root_ix = initialize_address_book_root_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root_pubkey,
        );
        send_legacy_transaction(
            &rpc,
            "initialize_address_book_root",
            &payer,
            &[&owner, &address_book_root],
            &[initialize_address_book_root_ix],
        )?;
        surfpool_log(format!(
            "bootstrap: initialized address book root {}",
            address_book_root_pubkey
        ));

        let create_owner_ata_ix =
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &payer.pubkey(),
                &owner.pubkey(),
                &USDC_MINT,
                &anchor_spl::token::ID,
            );
        send_legacy_transaction(
            &rpc,
            "create_owner_ata",
            &payer,
            &[],
            &[create_owner_ata_ix],
        )?;
        surfpool_log(format!("bootstrap: ensured owner ATA {}", owner_ata));

        let initialize_ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root_pubkey,
            &sra_config,
            handler_data.clone(),
            salt,
        );
        send_legacy_transaction(&rpc, "initialize_sra", &payer, &[&owner], &[initialize_ix])?;
        surfpool_log(format!("bootstrap: initialized SRA config {}", sra_config));

        let address_book_entry = derive_address_book_pda(
            &address_book_root_pubkey,
            &USDC_MINT,
            target_chain_id,
            &target_token,
        )
        .0;
        let address_book_ix = set_address_book_entry_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root_pubkey,
            &address_book_entry,
            USDC_MINT,
            target_chain_id,
            target_token,
            TARGET_TOKEN_DECIMALS,
        );
        send_legacy_transaction(
            &rpc,
            "set_address_book_entry",
            &payer,
            &[&owner],
            &[address_book_ix],
        )?;
        surfpool_log(format!(
            "bootstrap: created address book entry {} for chain {}",
            address_book_entry, target_chain_id
        ));

        let fixture = Self {
            rpc,
            http,
            payer,
            owner,
            address_book_root: address_book_root_pubkey,
            handler_data,
            sra_config,
            target_token,
            owner_ata,
            across_state,
            across_vault,
            accounts,
        };
        fixture.seed_owner_usdc(OWNER_STARTING_BALANCE)?;
        surfpool_log(format!(
            "bootstrap: seeded owner ATA {} with {} base units of USDC",
            fixture.owner_ata, OWNER_STARTING_BALANCE
        ));
        surfpool_log("bootstrap: fixture setup complete");
        Ok(fixture)
    }

    fn seed_owner_usdc(&self, amount: u64) -> TestResult<()> {
        self.surfpool_rpc(
            "surfnet_setTokenAccount",
            json!([
                self.owner.pubkey().to_string(),
                USDC_MINT.to_string(),
                { "amount": amount },
                anchor_spl::token::ID.to_string()
            ]),
        )?;
        Ok(())
    }

    fn surfpool_rpc(&self, method: &str, params: Value) -> TestResult<Value> {
        surfpool_log(format!("surfpool_rpc[{method}]: sending request"));
        let response: Value = self
            .http
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

    fn invoke_source_flow(&self, params: PrepareParams) -> TestResult<Vec<String>> {
        let current_nonce = self.current_nonce()?;
        surfpool_log(format!(
            "invoke_source_flow: starting for target_chain_id={} input_amount={} output_amount={} current_nonce={}",
            self.handler_data.target_chain_id,
            params.input_amount,
            params.output_amount,
            current_nonce
        ));
        let address_book_entry = derive_address_book_pda(
            &self.address_book_root,
            &USDC_MINT,
            self.handler_data.target_chain_id,
            &self.target_token,
        )
        .0;
        let execute_ix = execute_deposit_across_ix(
            &self.payer.pubkey(),
            &ValidatePrepareFixture {
                payer: self.payer.insecure_clone(),
                owner: self.owner.insecure_clone(),
                address_book_root: self.address_book_root,
                handler_data: self.handler_data.clone(),
                sra_config: self.sra_config,
                source_mint: USDC_MINT,
                target_chain_id: self.handler_data.target_chain_id,
                target_token: self.target_token,
                adapter_program: ADAPTER_PROGRAM_ID,
                accounts: self.accounts.clone(),
                owner_ata: self.owner_ata,
                across_state: self.across_state,
                across_vault: self.across_vault,
            },
            &read_anchor_account::<SraConfig>(&self.rpc, &self.sra_config)?,
            &params,
            AcrossDepositParams::from_prepare(
                &params,
                &self.handler_data,
                self.payer.pubkey(),
                FILL_DEADLINE_OFFSET_SECS,
                EXCLUSIVITY_PERIOD_SECS,
            ),
            current_nonce,
        );
        let validate_ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &self.address_book_root,
            &address_book_entry,
            self.handler_data.clone(),
            params.clone(),
            &execute_ix,
        );

        let lookup_addresses =
            representative_lookup_table(&[validate_ix.clone(), execute_ix.clone()]);
        let recent_slot: Slot = self.rpc.get_slot()?;
        let (create_lookup_table_ix, lookup_table_address) =
            create_lookup_table(self.payer.pubkey(), self.payer.pubkey(), recent_slot);
        surfpool_log(format!(
            "invoke_source_flow: creating lookup table {} at recent slot {} with {} address(es)",
            lookup_table_address,
            recent_slot,
            lookup_addresses.len()
        ));
        let extend_lookup_table_ix = extend_lookup_table(
            lookup_table_address,
            self.payer.pubkey(),
            Some(self.payer.pubkey()),
            lookup_addresses,
        );
        send_legacy_transaction(
            &self.rpc,
            "create_and_extend_lookup_table",
            &self.payer,
            &[],
            &[create_lookup_table_ix, extend_lookup_table_ix],
        )?;

        // Solana only activates newly extended ALT addresses in a later slot.
        let mut lookup_table = None;
        for attempt in 1..=SURFPOOL_RETRY_COUNT {
            let current_slot = self.rpc.get_slot()?;
            let (candidate_lookup_table, last_extended_slot) =
                lookup_table_account_from_rpc(&self.rpc, &lookup_table_address)?;
            surfpool_log(format!(
                "invoke_source_flow: ALT activation attempt {attempt}/{SURFPOOL_RETRY_COUNT} current_slot={current_slot} last_extended_slot={last_extended_slot}"
            ));
            if lookup_table_is_active(current_slot, last_extended_slot) {
                surfpool_log(format!(
                    "invoke_source_flow: ALT {lookup_table_address} active on attempt {attempt}/{SURFPOOL_RETRY_COUNT}"
                ));
                lookup_table = Some(candidate_lookup_table);
                break;
            }
            sleep(Duration::from_millis(200));
        }
        let lookup_table = lookup_table.ok_or("lookup table did not become active")?;

        let signature = send_versioned_transaction(
            &self.rpc,
            "source_flow",
            &self.payer,
            &[&self.owner],
            &[compute_budget_ix(), validate_ix, execute_ix],
            &[lookup_table],
        )?;

        let logs = self.logs_for_signature(&signature)?;
        surfpool_log(format!(
            "invoke_source_flow: fetched {} local log line(s) for signature {signature}",
            logs.len()
        ));
        Ok(logs)
    }

    fn current_nonce(&self) -> TestResult<u64> {
        match self.rpc.get_account(&self.accounts.mint_nonce) {
            Ok(account) => Ok(MintNonce::try_deserialize(&mut account.data.as_slice())?.nonce),
            Err(_) => Ok(0),
        }
    }

    fn mint_nonce(&self) -> TestResult<MintNonce> {
        read_anchor_account(&self.rpc, &self.accounts.mint_nonce)
    }

    fn owner_token_amount(&self) -> TestResult<u64> {
        Ok(self
            .rpc
            .get_token_account_balance(&self.owner_ata)?
            .amount
            .parse()?)
    }

    fn sra_vault_amount(&self) -> TestResult<u64> {
        Ok(self
            .rpc
            .get_token_account_balance(&self.accounts.sra_vault)?
            .amount
            .parse()?)
    }

    fn adapter_custody_amount(&self) -> TestResult<u64> {
        Ok(self
            .rpc
            .get_token_account_balance(&self.accounts.adapter_custody)?
            .amount
            .parse()?)
    }

    fn across_vault_amount(&self) -> TestResult<u64> {
        Ok(self
            .rpc
            .get_token_account_balance(&self.across_vault)?
            .amount
            .parse()?)
    }

    fn logs_for_signature(&self, signature: &Signature) -> TestResult<Vec<String>> {
        let local = self.surfpool_rpc("surfnet_getLocalSignatures", json!([20]))?;
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
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_across_happy_path() {
    let _guard = SURFPOOL_TEST_MUTEX.lock().expect("lock surfpool mutex");
    let fx = SurfpoolSourceFixture::bootstrap().expect("bootstrap fixture");
    let pre_owner = fx
        .owner_token_amount()
        .expect("read owner ATA before deposit");
    let pre_vault = fx
        .across_vault_amount()
        .expect("read Across vault before deposit");
    surfpool_log(format!(
        "source_e2e_real_across_happy_path: pre balances owner={} across_vault={}",
        pre_owner, pre_vault
    ));

    let logs = fx
        .invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("paired source flow should succeed against real Across");
    let post_owner = fx
        .owner_token_amount()
        .expect("read owner ATA after deposit");
    let post_sra_vault = fx.sra_vault_amount().expect("read SRA vault after deposit");
    let post_adapter_custody = fx
        .adapter_custody_amount()
        .expect("read adapter custody after deposit");
    let post_across_vault = fx
        .across_vault_amount()
        .expect("read Across vault after deposit");
    surfpool_log(format!(
        "source_e2e_real_across_happy_path: post balances owner={} sra_vault={} adapter_custody={} across_vault={} nonce={}",
        post_owner,
        post_sra_vault,
        post_adapter_custody,
        post_across_vault,
        fx.mint_nonce()
            .expect("read mint nonce after deposit")
            .nonce,
    ));

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
        "expected ≥2 Across invocations (deposit_now + emit_cpi event), got {across_invokes}"
    );
    assert!(
        across_successes >= 2,
        "expected ≥2 Across success lines (deposit_now + emit_cpi event), got {across_successes}"
    );
    assert_eq!(post_owner, pre_owner - INPUT_AMOUNT);
    assert_eq!(post_sra_vault, 0);
    assert_eq!(post_adapter_custody, 0);
    assert_eq!(post_across_vault, pre_vault + INPUT_AMOUNT);
    assert_eq!(
        fx.mint_nonce()
            .expect("read mint nonce after deposit")
            .nonce,
        1
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_across_nonce_increments() {
    let _guard = SURFPOOL_TEST_MUTEX.lock().expect("lock surfpool mutex");
    let fx = SurfpoolSourceFixture::bootstrap().expect("bootstrap fixture");
    surfpool_log("source_e2e_real_across_nonce_increments: running first deposit");

    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("first deposit succeeds");
    surfpool_log("source_e2e_real_across_nonce_increments: running second deposit");
    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("second deposit succeeds");

    surfpool_log(format!(
        "source_e2e_real_across_nonce_increments: final nonce={}",
        fx.mint_nonce()
            .expect("read mint nonce after second deposit")
            .nonce,
    ));

    assert_eq!(
        fx.mint_nonce()
            .expect("read mint nonce after second deposit")
            .nonce,
        2
    );
}
