mod common;

use anchor_lang::AccountDeserialize;
use anchor_spl::associated_token::spl_associated_token_account;
use common::surfpool::*;
use common::*;
use reqwest::blocking::Client;
use serde_json::json;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
};
use sra_common::abi::u256_from_u64;
use sra_common::{
    constants::{ACROSS_SPOKE_POOL_ID, USDC_MINT},
    source_deposit::{AcrossDepositParams, PrepareParams},
    types::{MintNonce, SourceHandlerData, SraConfig},
};

const INPUT_AMOUNT: u64 = 1_000_000;
// Quoted output must remain within `SLIPPAGE_BPS` of `INPUT_AMOUNT`, i.e.
// >= INPUT_AMOUNT * (BPS_DENOMINATOR - SLIPPAGE_BPS) / BPS_DENOMINATOR.
// 1_000_000 * 9_950 / 10_000 = 995_000, so 999_500 is comfortably within.
const OUTPUT_WITHIN_SLIPPAGE: u64 = 999_500;
const TARGET_TOKEN_DECIMALS: u8 = 6;
// Across deposit window: 1h fill deadline, 15m exclusive-relayer window.
const FILL_DEADLINE_OFFSET_SECS: u32 = 3_600;
const EXCLUSIVITY_PERIOD_SECS: u32 = 900;

fn default_prepare_params_rpc(fx: &SurfpoolSourceFixture) -> PrepareParams {
    let mut params = sample_prepare_params(fx.target_token);
    params.input_amount = INPUT_AMOUNT;
    params.output_amount = u256_from_u64(OUTPUT_WITHIN_SLIPPAGE);
    params
}

// Pins the SHARED `common::surfpool::lookup_table_is_active` helper. It lives
// here (not in `common/`) because a `#[test]` in `common/` would re-run in
// every test binary that includes the module.
#[test]
fn lookup_table_activation_waits_for_last_extended_slot() {
    assert!(!lookup_table_is_active(10, 10));
    assert!(!lookup_table_is_active(10, 11));
    assert!(lookup_table_is_active(12, 11));
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

        let http = http_client()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let address_book_root = Keypair::new();
        let address_book_root_pubkey = address_book_root.pubkey();
        let handler_data = sample_handler_data(address_book_root_pubkey);
        let target_token = sample_target_token();
        let target_chain_id = sample_prepare_params(target_token).target_chain_id;
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

        request_and_confirm_airdrop(&rpc, "payer", &payer.pubkey(), 10 * SOL)?;
        request_and_confirm_airdrop(&rpc, "owner", &owner.pubkey(), SOL)?;

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

        // the fee config singleton is a REQUIRED deposit account. The
        // forked mainnet ledger has no singleton for our (dev-keyed) program,
        // so initialize it with the disabled (default) recipient — deposits
        // then exercise the genesis-like zero-fee path. Skip when an earlier
        // E2E run on the same fork already created it (first-caller-wins).
        if rpc.get_account(&derive_fee_config_pda().0).is_err() {
            let fee_admin = Keypair::new();
            let initialize_fee_config =
                initialize_fee_config_ix(&payer.pubkey(), &fee_admin.pubkey(), Pubkey::default());
            send_legacy_transaction(
                &rpc,
                "initialize_fee_config",
                &payer,
                &[&fee_admin],
                &[initialize_fee_config],
            )?;
            surfpool_log("bootstrap: initialized fee config singleton (fees disabled)");
        } else {
            surfpool_log("bootstrap: fee config singleton already present");
        }

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
        surfpool_rpc(
            &self.http,
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

    fn invoke_source_flow(&self, params: PrepareParams) -> TestResult<Vec<String>> {
        let current_nonce = self.current_nonce()?;
        surfpool_log(format!(
            "invoke_source_flow: starting for target_chain_id={} input_amount={} output_amount={:?} current_nonce={}",
            params.target_chain_id,
            params.input_amount,
            params.output_amount,
            current_nonce
        ));
        let address_book_entry = derive_address_book_pda(
            &self.address_book_root,
            &USDC_MINT,
            params.target_chain_id,
            &params.target_token,
        )
        .0;
        let execute_ix = execute_deposit_across_ix(
            &self.payer.pubkey(),
            &ValidatePrepareFixture {
                payer: self.payer.insecure_clone(),
                owner: self.owner.insecure_clone(),
                fee_admin: Keypair::new(),
                address_book_root: self.address_book_root,
                handler_data: self.handler_data.clone(),
                sra_config: self.sra_config,
                source_mint: USDC_MINT,
                target_chain_id: params.target_chain_id,
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
            )
            .expect("prepare params must produce a valid net amount"),
        );
        let validate_ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &self.address_book_root,
            &address_book_entry,
            None,
            self.handler_data.clone(),
            params.clone(),
            &execute_ix,
        );

        let lookup_addresses =
            representative_lookup_addresses(&[validate_ix.clone(), execute_ix.clone()]);
        let lookup_table = prepare_lookup_table(&self.rpc, &self.payer, lookup_addresses)?;

        let signature = send_versioned_transaction(
            &self.rpc,
            "source_flow",
            &self.payer,
            &[&self.owner],
            &[compute_budget_ix(), validate_ix, execute_ix],
            &[lookup_table],
        )?;

        let logs = logs_for_signature(&self.http, &signature)?;
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
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_across_happy_path() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
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
    let _lock = surfpool_lock().expect("acquire surfpool lock");
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
