mod common;

use anchor_lang::AccountDeserialize;
use anchor_spl::associated_token::spl_associated_token_account;
use common::*;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signature, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use sra_common::{
    constants::{ACROSS_SPOKE_POOL_ID, USDC_MINT},
    source_deposit::{AcrossDepositParams, PrepareParams},
    test_utils::DEFAULT_ACTION_IDXS,
    types::{MintNonce, SraConfig},
};
use sra_handler::instructions::source::initialize_sra::SraInitConfig;
use std::{env, error::Error, path::PathBuf, sync::Mutex, thread::sleep, time::Duration};

const INPUT_AMOUNT: u64 = 1_000_000;
const SLIPPAGE_BPS: u16 = 50;
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

fn default_prepare_params_rpc(fx: &SurfpoolSourceFixture) -> PrepareParams {
    PrepareParams::new(
        INPUT_AMOUNT,
        OUTPUT_WITHIN_SLIPPAGE,
        fx.target_token,
        DEFAULT_ACTION_IDXS.to_vec(),
    )
}

fn payer_keypair_path() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME must be set")).join(".config/solana/id.json")
}

fn wait_for_surfpool(rpc: &RpcClient) -> TestResult<()> {
    for _ in 0..SURFPOOL_RETRY_COUNT {
        if rpc.get_latest_blockhash().is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(SURFPOOL_RETRY_DELAY_MS));
    }

    Err("Surfpool RPC did not become ready".into())
}

fn wait_for_program_deploys(rpc: &RpcClient, program_ids: &[Pubkey]) -> TestResult<()> {
    for _ in 0..SURFPOOL_RETRY_COUNT {
        if program_ids
            .iter()
            .all(|program_id| rpc.get_account(program_id).is_ok())
        {
            return Ok(());
        }
        sleep(Duration::from_millis(SURFPOOL_RETRY_DELAY_MS));
    }

    Err("Surfpool did not finish auto-deploying local programs".into())
}

fn rpc_client() -> RpcClient {
    RpcClient::new_with_commitment(SURFPOOL_RPC_URL.to_string(), CommitmentConfig::confirmed())
}

fn send_legacy_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[solana_sdk::instruction::Instruction],
) -> TestResult<Signature> {
    let blockhash = rpc.get_latest_blockhash()?;
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all_signers, blockhash);
    Ok(rpc.send_and_confirm_transaction(&tx)?)
}

fn send_versioned_transaction(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[solana_sdk::instruction::Instruction],
) -> TestResult<Signature> {
    let blockhash = rpc.get_latest_blockhash()?;
    let message = v0::Message::try_compile(&payer.pubkey(), ixs, &[], blockhash)?;
    let mut all_signers = vec![payer];
    all_signers.extend_from_slice(signers);
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &all_signers)?;
    Ok(rpc.send_and_confirm_transaction(&tx)?)
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
    config: SraInitConfig,
    sra_config: Pubkey,
    target_token: [u8; 32],
    owner_ata: Pubkey,
    across_state: Pubkey,
    across_vault: Pubkey,
    accounts: ValidatePrepareAccounts,
}

impl SurfpoolSourceFixture {
    fn bootstrap() -> TestResult<Self> {
        let rpc = rpc_client();
        wait_for_surfpool(&rpc)?;

        let http = Client::builder()
            .timeout(Duration::from_secs(SURFPOOL_HTTP_TIMEOUT_SECS))
            .build()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let config = sample_config();
        let salt = [0xAB; 32];
        let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &config, &salt);
        let accounts =
            ValidatePrepareAccounts::derive(&sra_config, &USDC_MINT, &ADAPTER_PROGRAM_ID);
        let target_token = sample_target_token();
        let owner_ata = derive_ata(&owner.pubkey(), &USDC_MINT);
        let (across_state, _) = derive_real_across_state_pda();
        let across_vault = derive_ata(&across_state, &USDC_MINT);

        wait_for_program_deploys(&rpc, &[PROGRAM_ID, ADAPTER_PROGRAM_ID])?;
        rpc.get_account(&ACROSS_SPOKE_POOL_ID)?;

        let payer_airdrop = rpc.request_airdrop(&payer.pubkey(), 10 * SOL)?;
        rpc.confirm_transaction(&payer_airdrop)?;

        let owner_airdrop = rpc.request_airdrop(&owner.pubkey(), SOL)?;
        rpc.confirm_transaction(&owner_airdrop)?;

        let create_owner_ata_ix =
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &payer.pubkey(),
                &owner.pubkey(),
                &USDC_MINT,
                &anchor_spl::token::ID,
            );
        send_legacy_transaction(&rpc, &payer, &[], &[create_owner_ata_ix])?;

        let initialize_ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            config.clone(),
            salt,
        );
        send_legacy_transaction(&rpc, &payer, &[&owner], &[initialize_ix])?;

        let address_book_entry =
            derive_address_book_pda(&sra_config, config.target_chain_id, &target_token).0;
        let address_book_ix = set_address_book_entry_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            &address_book_entry,
            config.target_chain_id,
            target_token,
            TARGET_TOKEN_DECIMALS,
        );
        send_legacy_transaction(&rpc, &payer, &[&owner], &[address_book_ix])?;

        let update_adapters_ix = update_adapters_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &sra_config,
            vec![ADAPTER_PROGRAM_ID],
        );
        send_legacy_transaction(&rpc, &payer, &[&owner], &[update_adapters_ix])?;

        let update_slippage_ix = update_slippage_ix(&owner.pubkey(), &sra_config, SLIPPAGE_BPS);
        send_legacy_transaction(&rpc, &payer, &[&owner], &[update_slippage_ix])?;

        let fixture = Self {
            rpc,
            http,
            payer,
            owner,
            config,
            sra_config,
            target_token,
            owner_ata,
            across_state,
            across_vault,
            accounts,
        };
        fixture.seed_owner_usdc(OWNER_STARTING_BALANCE)?;
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
            return Err(format!("{method} failed: {err}").into());
        }

        Ok(response["result"].clone())
    }

    fn invoke_source_flow(&self, params: PrepareParams) -> TestResult<Vec<String>> {
        let address_book_entry = derive_address_book_pda(
            &self.sra_config,
            self.config.target_chain_id,
            &params.target_token,
        )
        .0;
        let validate_ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &address_book_entry,
            params.clone(),
        );
        let execute_ix = execute_deposit_across_ix(
            &self.payer.pubkey(),
            &ValidatePrepareFixture {
                payer: self.payer.insecure_clone(),
                owner: self.owner.insecure_clone(),
                config: self.config.clone(),
                sra_config: self.sra_config,
                source_mint: USDC_MINT,
                target_chain_id: self.config.target_chain_id,
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
                self.payer.pubkey(),
                FILL_DEADLINE_OFFSET_SECS,
                EXCLUSIVITY_PERIOD_SECS,
            ),
            self.current_nonce()?,
        );
        let signature = send_versioned_transaction(
            &self.rpc,
            &self.payer,
            &[&self.owner],
            &[compute_budget_ix(), validate_ix, execute_ix],
        )?;

        self.logs_for_signature(&signature)
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
    let _guard = SURFPOOL_TEST_MUTEX
        .lock()
        .expect("lock surfpool mutex");
    let fx = SurfpoolSourceFixture::bootstrap().expect("bootstrap fixture");
    let pre_owner = fx
        .owner_token_amount()
        .expect("read owner ATA before deposit");
    let pre_vault = fx
        .across_vault_amount()
        .expect("read Across vault before deposit");

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
    let _guard = SURFPOOL_TEST_MUTEX
        .lock()
        .expect("lock surfpool mutex");
    let fx = SurfpoolSourceFixture::bootstrap().expect("bootstrap fixture");

    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("first deposit succeeds");
    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("second deposit succeeds");

    assert_eq!(
        fx.mint_nonce()
            .expect("read mint nonce after second deposit")
            .nonce,
        2
    );
}
