//! Source E2E against the REAL bridge programs on a Surfpool mainnet fork:
//! the Across SVM Spoke Pool and the CCTP v2 TokenMessengerMinter /
//! MessageTransmitter. One fixture drives the paired
//! `trigger` + `execute_deposit_*` v0 transaction (compute
//! budget + ALT) for whichever adapter it was bootstrapped with.
//!
//! KEEPER SPEC (Solana → EVM over CCTP; this suite is the reference for the
//! on-chain leg): (1) submit the paired deposit transaction below — the
//! ephemeral `message_sent_event_data` keypair co-signs; (2) poll Iris
//! `GET /v2/messages/5?transactionHash=<sig>` until `status == complete`;
//! (3) submit `receiveMessage(message, attestation)` on the destination EVM
//! MessageTransmitter — requires a funded EVM wallet (ops requirement);
//! (4) after ≥ 5 days, optionally `reclaim_event_account(attestation,
//! destination_message)` to recover the ~0.004 SOL event-account rent
//! (payee must be the original fee payer).

mod common;

use anchor_lang::AccountSerialize;
use common::surfpool::*;
use common::*;
use reqwest::blocking::Client;
use serde_json::json;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
};
use sra_cctp_adapter::utils::message::{BurnMessage, CctpMessage};
use sra_common::abi::u256_from_u64;
use sra_common::{
    constants::{
        cctp_domain_for_chain, ACROSS_SPOKE_POOL_ID, CCTP_MESSAGE_TRANSMITTER_V2_ID,
        CCTP_TOKEN_MESSENGER_MINTER_V2_ID, FINALITY_THRESHOLD_FINALIZED, SOLANA_CCTP_DOMAIN,
        USDC_MINT,
    },
    source_deposit::{AcrossDepositParams, PrepareParams},
    types::{FeeConfig, SourceHandlerData, SraConfig},
};

const INPUT_AMOUNT: u64 = 1_000_000;
const PROTOCOL_FEE: u64 = 3_000;
const NET_AMOUNT: u64 = INPUT_AMOUNT - PROTOCOL_FEE;
const TARGET_TOKEN_DECIMALS: u8 = 6;
// Across deposit window: 1h fill deadline, 15m exclusive-relayer window.
const FILL_DEADLINE_OFFSET_SECS: u32 = 3_600;
const EXCLUSIVITY_PERIOD_SECS: u32 = 900;

fn default_prepare_params_rpc(fx: &SurfpoolSourceFixture) -> PrepareParams {
    let mut params = sample_prepare_params(fx.target_token);
    params.input_amount = INPUT_AMOUNT;
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
    initiator: Keypair,
    address_book_root: Pubkey,
    handler_data: SourceHandlerData,
    sra_config: Pubkey,
    target_token: [u8; 32],
    across_state: Pubkey,
    across_vault: Pubkey,
    adapter_program: Pubkey,
    accounts: TriggerAccounts,
    fee_ata: Pubkey,
}

impl SurfpoolSourceFixture {
    /// Bootstrap an SRA whose `handler_data` allowlists exactly
    /// `adapter_program` — the same fresh-owner/fresh-root setup for either
    /// adapter, differing only in the bridge whose fork presence is checked.
    fn bootstrap(adapter_program: Pubkey, fee_bps: u16) -> TestResult<Self> {
        surfpool_log("bootstrap: starting Surfpool source fixture setup");
        let rpc = rpc_client();
        wait_for_surfpool(&rpc)?;
        surfpool_log("bootstrap: Surfpool RPC is ready");

        let http = http_client()?;
        let payer = read_keypair_file(payer_keypair_path())?;
        let owner = Keypair::new();
        let initiator = Keypair::new();
        let address_book_root = Keypair::new();
        let address_book_root_pubkey = address_book_root.pubkey();
        let mut handler_data = sample_handler_data(address_book_root_pubkey);
        handler_data.allowed_adapters = vec![adapter_program];
        let target_token = sample_target_token();
        let target_chain_id = sample_prepare_params(target_token).target_chain_id;
        let salt = [0xAB; 32];
        let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
        let accounts = TriggerAccounts::derive(&sra_config, &USDC_MINT, &adapter_program);
        let (across_state, _) = derive_real_across_state_pda();
        let across_vault = derive_ata(&across_state, &USDC_MINT);

        wait_for_program_deploys(&rpc, &[PROGRAM_ID, adapter_program])?;
        surfpool_log("bootstrap: local programs are deployed");
        if adapter_program == ADAPTER_PROGRAM_ID {
            rpc.get_account(&ACROSS_SPOKE_POOL_ID)?;
            surfpool_log(format!(
                "bootstrap: Across spoke pool {ACROSS_SPOKE_POOL_ID} is reachable"
            ));
            unpause_across_spoke_state(&rpc, &http, &across_state)?;
        } else {
            rpc.get_account(&CCTP_TOKEN_MESSENGER_MINTER_V2_ID)?;
            rpc.get_account(&CCTP_MESSAGE_TRANSMITTER_V2_ID)?;
            surfpool_log("bootstrap: CCTP v2 programs are reachable");
        }

        request_and_confirm_airdrop(&rpc, "payer", &payer.pubkey(), 10 * SOL)?;
        request_and_confirm_airdrop(&rpc, "owner", &owner.pubkey(), SOL)?;
        request_and_confirm_airdrop(&rpc, "initiator", &initiator.pubkey(), SOL)?;

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

        let fee_config = derive_fee_config_pda().0;
        if rpc.get_account(&fee_config).is_ok() {
            let mut config: FeeConfig = read_anchor_account(&rpc, &fee_config)?;
            config.admin = payer.pubkey();
            let mut data = Vec::new();
            config.try_serialize(&mut data)?;
            let hex_data: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            surfpool_rpc(
                &http,
                "surfnet_setAccount",
                json!([fee_config.to_string(), { "data": hex_data }]),
            )?;
        } else {
            send_legacy_transaction(
                &rpc,
                "initialize_fee_config",
                &payer,
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
        send_legacy_transaction(
            &rpc,
            "configure_protocol_fee",
            &payer,
            &[],
            &[
                set_fee_recipient_ix(&payer.pubkey(), fee_recipient),
                set_protocol_fee_ix(&payer.pubkey(), &payer.pubkey(), fee_bps, false, false),
            ],
        )?;
        let fee_ata = derive_ata(&fee_recipient, &USDC_MINT);
        if fee_bps != 0 {
            surfpool_rpc(
                &http,
                "surfnet_setTokenAccount",
                json!([fee_recipient.to_string(), USDC_MINT.to_string(), { "amount": 0 }, anchor_spl::token::ID.to_string()]),
            )?;
        }

        let initialize_ix = initialize_sra_ix(
            &payer.pubkey(),
            &owner.pubkey(),
            &address_book_root_pubkey,
            &sra_config,
            handler_data.clone(),
            salt,
            initiator.pubkey(),
            USDC_MINT,
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
            initiator,
            address_book_root: address_book_root_pubkey,
            handler_data,
            sra_config,
            target_token,
            across_state,
            across_vault,
            adapter_program,
            accounts,
            fee_ata,
        };
        surfpool_rpc(
            &fixture.http,
            "surfnet_setTokenAccount",
            json!([
                fixture.sra_config.to_string(),
                USDC_MINT.to_string(),
                { "amount": 2_000_000 },
                anchor_spl::token::ID.to_string()
            ]),
        )?;
        assert_eq!(
            fixture.token_amount(&fixture.accounts.sra_vault)?,
            2_000_000
        );
        Ok(fixture)
    }

    fn passive_fixture(&self, target_chain_id: u64) -> PassiveFixture {
        PassiveFixture::from_existing_accounts(
            &self.payer,
            &self.owner,
            &self.initiator,
            self.handler_data.clone(),
            self.sra_config,
            USDC_MINT,
            target_chain_id,
            self.adapter_program,
            self.across_state,
            self.across_vault,
        )
    }

    /// Pair `trigger` with the given adapter leg and submit the
    /// production shape: one v0 transaction, compute budget first, all
    /// non-signer accounts resolved through a real address lookup table.
    fn submit_source_flow(
        &self,
        params: &PrepareParams,
        execute_ix: solana_sdk::instruction::Instruction,
        extra_signers: &[&Keypair],
    ) -> TestResult<Vec<String>> {
        surfpool_log(format!(
            "submit_source_flow: starting for target_chain_id={} input_amount={} output_amount={:?} current_nonce={}",
            params.target_chain_id,
            params.input_amount,
            params.output_amount,
            self.current_nonce()?
        ));
        let address_book_entry = derive_address_book_pda(
            &self.address_book_root,
            &USDC_MINT,
            params.target_chain_id,
            &params.target_token,
        )
        .0;
        let trigger = trigger_ix(
            &self.payer.pubkey(),
            &self.initiator.pubkey(),
            &self.accounts,
            &address_book_entry,
            (params.protocol_fee_amount != 0).then_some(self.fee_ata),
            params.clone(),
            &execute_ix,
        );

        let lookup_addresses =
            representative_lookup_addresses(&[trigger.clone(), execute_ix.clone()]);
        let lookup_table = prepare_lookup_table(&self.rpc, &self.payer, lookup_addresses)?;

        let mut signers = vec![&self.initiator];
        signers.extend_from_slice(extra_signers);
        let tx = build_versioned_transaction(
            &self.rpc,
            "source_flow",
            &self.payer,
            &signers,
            &[compute_budget_ix(), trigger, execute_ix],
            &[lookup_table],
        )?;
        let signer_keys = &tx.message.static_account_keys()
            [..usize::from(tx.message.header().num_required_signatures)];
        let mut expected_signers = vec![self.payer.pubkey(), self.initiator.pubkey()];
        expected_signers.extend(extra_signers.iter().map(|signer| signer.pubkey()));
        assert!(!signer_keys.contains(&self.owner.pubkey()));
        assert_eq!(signer_keys.len(), expected_signers.len());
        assert!(expected_signers.iter().all(|key| signer_keys.contains(key)));
        let signature = self.rpc.send_and_confirm_transaction(&tx)?;

        let logs = logs_for_signature(&self.http, &signature)?;
        surfpool_log(format!(
            "submit_source_flow: fetched {} local log line(s) for signature {signature}",
            logs.len()
        ));
        Ok(logs)
    }

    fn invoke_source_flow(&self, params: PrepareParams) -> TestResult<Vec<String>> {
        let execute_ix = execute_deposit_across_ix(
            &self.payer.pubkey(),
            &self.passive_fixture(params.target_chain_id),
            &params,
            AcrossDepositParams::from_prepare(
                &params,
                self.payer.pubkey(),
                FILL_DEADLINE_OFFSET_SECS,
                EXCLUSIVITY_PERIOD_SECS,
            )
            .expect("prepare params must produce a valid net amount"),
        );
        self.submit_source_flow(&params, execute_ix, &[])
    }

    /// The CCTP paired flow: the ephemeral `message_sent_event_data` keypair
    /// co-signs, and its pubkey is returned so tests can read back the real
    /// `MessageSent` account the transmitter wrote.
    fn invoke_source_flow_cctp(&self, params: PrepareParams) -> TestResult<(Vec<String>, Pubkey)> {
        let (execute_ix, event_data) = execute_deposit_cctp_ix(
            &self.payer.pubkey(),
            &self.passive_fixture(params.target_chain_id),
            &cctp_params_for(&params),
        );
        let logs = self.submit_source_flow(&params, execute_ix, &[&event_data])?;
        Ok((logs, event_data.pubkey()))
    }

    fn current_nonce(&self) -> TestResult<u64> {
        Ok(read_anchor_account::<SraConfig>(&self.rpc, &self.sra_config)?.deposit_nonce)
    }

    fn token_amount(&self, account: &Pubkey) -> TestResult<u64> {
        Ok(self
            .rpc
            .get_token_account_balance(account)?
            .amount
            .parse()?)
    }

    /// Total USDC supply on the fork — the CCTP deposit is a BURN, asserted
    /// as a supply decrease (Across, by contrast, escrows into its vault).
    fn usdc_supply(&self) -> TestResult<u64> {
        Ok(self.rpc.get_token_supply(&USDC_MINT)?.amount.parse()?)
    }
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_across_passive_deposit() -> TestResult<()> {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolSourceFixture::bootstrap(ADAPTER_PROGRAM_ID, 30)?;
    let mut params = default_prepare_params_rpc(&fx);
    params.protocol_fee_amount = PROTOCOL_FEE;
    params.output_amount = u256_from_u64(995_000);
    let pre_sra_vault = fx.token_amount(&fx.accounts.sra_vault)?;
    let pre_vault = fx.token_amount(&fx.across_vault)?;
    let pre_fee = fx.token_amount(&fx.fee_ata)?;
    let pre_nonce = fx.current_nonce()?;
    let logs = fx.invoke_source_flow(params)?;
    assert_program_ran(
        &logs,
        "Across spoke pool (deposit_now + emit_cpi event)",
        &ACROSS_SPOKE_POOL_ID,
        2,
    );
    assert_eq!(
        fx.token_amount(&fx.accounts.sra_vault)?,
        pre_sra_vault - INPUT_AMOUNT
    );
    assert_eq!(fx.token_amount(&fx.accounts.adapter_custody)?, 0);
    assert_eq!(fx.token_amount(&fx.across_vault)?, pre_vault + NET_AMOUNT);
    assert_eq!(fx.token_amount(&fx.fee_ata)?, pre_fee + PROTOCOL_FEE);
    assert_eq!(fx.current_nonce()?, pre_nonce + 1);
    Ok(())
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_across_nonce_increments() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolSourceFixture::bootstrap(ADAPTER_PROGRAM_ID, 0).expect("bootstrap fixture");
    surfpool_log("source_e2e_real_across_nonce_increments: running first deposit");

    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("first deposit succeeds");
    surfpool_log("source_e2e_real_across_nonce_increments: running second deposit");
    fx.invoke_source_flow(default_prepare_params_rpc(&fx))
        .expect("second deposit succeeds");

    surfpool_log(format!(
        "source_e2e_real_across_nonce_increments: final nonce={}",
        fx.current_nonce()
            .expect("read SRA nonce after second deposit"),
    ));

    assert_eq!(
        fx.current_nonce()
            .expect("read SRA nonce after second deposit"),
        2
    );
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_cctp_passive_deposit() -> TestResult<()> {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx = SurfpoolSourceFixture::bootstrap(CCTP_ADAPTER_PROGRAM_ID, 30)?;
    let mut params = default_prepare_params_rpc(&fx);
    params.protocol_fee_amount = PROTOCOL_FEE;
    params.output_amount = u256_from_u64(995_000);
    let pre_sra_vault = fx.token_amount(&fx.accounts.sra_vault)?;
    let pre_supply = fx.usdc_supply()?;
    let pre_fee = fx.token_amount(&fx.fee_ata)?;
    let pre_nonce = fx.current_nonce()?;

    let (logs, message_sent) = fx
        .invoke_source_flow_cctp(params.clone())
        .expect("paired source flow should succeed against real CCTP v2");

    // The real TokenMessengerMinter ran (deposit_for_burn) and CPI'd the real
    // MessageTransmitter (send_message) — a burn, not a vault transfer.
    assert_program_ran(
        &logs,
        "TokenMessengerMinter",
        &CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        1,
    );
    assert_program_ran(
        &logs,
        "MessageTransmitter",
        &CCTP_MESSAGE_TRANSMITTER_V2_ID,
        1,
    );
    assert_eq!(
        fx.token_amount(&fx.accounts.sra_vault)?,
        pre_sra_vault - INPUT_AMOUNT
    );
    assert_eq!(fx.token_amount(&fx.accounts.adapter_custody)?, 0);
    assert_eq!(
        fx.usdc_supply()?,
        pre_supply - NET_AMOUNT,
        "deposit_for_burn must reduce the USDC supply by the net amount"
    );
    assert_eq!(fx.token_amount(&fx.fee_ata)?, pre_fee + PROTOCOL_FEE);

    // The ephemeral MessageSent account the real transmitter wrote: parse the
    // wire message with the adapter's own parsers and pin every field the
    // source leg controls (the nonce stays zeroed — Iris assigns it off-chain
    // at attestation time).
    let event_account = fx
        .rpc
        .get_account(&message_sent)
        .expect("MessageSent account exists");
    assert_eq!(event_account.owner, CCTP_MESSAGE_TRANSMITTER_V2_ID);
    let event: mock_cctp::MessageSent = read_anchor_account(&fx.rpc, &message_sent)
        .expect("MessageSent account parses (real layout == mock layout)");
    assert_eq!(event.rent_payer, fx.payer.pubkey());
    let message = CctpMessage::parse(&event.message).expect("wire message parses");
    let expected_domain =
        cctp_domain_for_chain(params.target_chain_id).expect("target chain has a CCTP domain");
    let remote_token_messenger = cctp_remote_token_messenger(&fx.rpc, expected_domain)
        .expect("read the trusted remote token messenger from the fork");
    assert_eq!(message.version(), cctp::CCTP_HEADER_VERSION);
    assert_eq!(message.source_domain(), SOLANA_CCTP_DOMAIN);
    assert_eq!(message.destination_domain(), expected_domain);
    assert_eq!(message.destination_caller(), Pubkey::default());
    assert_eq!(
        message.min_finality_threshold(),
        FINALITY_THRESHOLD_FINALIZED
    );
    assert_eq!(message.finality_threshold_executed(), 0);
    assert_eq!(
        message.nonce(),
        [0u8; 32],
        "nonce is zero-filled at burn time"
    );
    assert_eq!(
        message.recipient(),
        Pubkey::new_from_array(remote_token_messenger),
        "recipient must be the destination domain's TokenMessenger"
    );
    let body = BurnMessage::parse(message.body()).expect("burn body parses");
    assert_eq!(
        body.version(),
        sra_cctp_adapter::utils::message::BURN_MESSAGE_BODY_VERSION
    );
    assert_eq!(body.burn_token(), USDC_MINT.to_bytes());
    assert_eq!(
        body.mint_recipient(),
        Pubkey::new_from_array(params.recipient)
    );
    assert_eq!(body.amount().expect("amount slot"), NET_AMOUNT);
    assert_eq!(body.max_fee().expect("max_fee slot"), 0);
    assert_eq!(body.fee_executed().expect("fee_executed slot"), 0);

    assert_eq!(fx.current_nonce()?, pre_nonce + 1);
    Ok(())
}

#[test]
#[ignore = "requires Surfpool at http://127.0.0.1:8899"]
fn source_e2e_real_cctp_nonce_increments() {
    let _lock = surfpool_lock().expect("acquire surfpool lock");
    let fx =
        SurfpoolSourceFixture::bootstrap(CCTP_ADAPTER_PROGRAM_ID, 0).expect("bootstrap fixture");
    surfpool_log("source_e2e_real_cctp_nonce_increments: running first deposit");

    let (_, first_event) = fx
        .invoke_source_flow_cctp(default_prepare_params_rpc(&fx))
        .expect("first deposit succeeds");
    surfpool_log("source_e2e_real_cctp_nonce_increments: running second deposit");
    let (_, second_event) = fx
        .invoke_source_flow_cctp(default_prepare_params_rpc(&fx))
        .expect("second deposit succeeds");

    assert_ne!(
        first_event, second_event,
        "each deposit writes its own ephemeral MessageSent account"
    );
    assert_eq!(
        fx.current_nonce()
            .expect("read SRA nonce after second deposit"),
        2
    );
}
