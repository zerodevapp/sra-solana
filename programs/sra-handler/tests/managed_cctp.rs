mod common;

use anchor_lang::{
    AccountDeserialize, AccountSerialize, AnchorSerialize, InstructionData, ToAccountMetas,
};
use common::cctp::derive_execution_lane;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use sra_cctp_adapter::state::{ActiveFill, DestinationPolicy, ExecutionLane, SourcePolicy};
use sra_common::{constants::USDC_MINT, types::SourceHandlerData};

const SALT: [u8; 32] = [11; 32];
const REMOTE_USDC: [u8; 32] =
    hex_literal::hex!("000000000000000000000000af88d065e77c8cc2239327c5edb3a432268e5831");

fn policy_pda(seed: &[u8], handler: Pubkey, salt: [u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[seed, handler.as_ref(), USDC_MINT.as_ref(), &salt],
        &CCTP_ADAPTER_PROGRAM_ID,
    )
    .0
}

fn raw_ix(name: &str, accounts: Vec<AccountMeta>, args: impl AnchorSerialize) -> Instruction {
    let mut data =
        solana_sdk::hash::hash(format!("global:{name}").as_bytes()).to_bytes()[..8].to_vec();
    args.serialize(&mut data).unwrap();
    Instruction {
        program_id: CCTP_ADAPTER_PROGRAM_ID,
        accounts,
        data,
    }
}

struct Fixture {
    payer: Keypair,
    owner: Keypair,
    executor: Keypair,
    sra: Pubkey,
    root: Pubkey,
    entry: Pubkey,
    handler_data: SourceHandlerData,
}

impl Fixture {
    fn new(svm: &mut litesvm::LiteSVM) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);
        let executor = funded_keypair(svm, SOL);
        let mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        svm.set_account(USDC_MINT, svm.get_account(&mint).unwrap())
            .unwrap();
        let root = init_address_book_root(svm, &payer, &owner);
        let mut handler_data = sample_handler_data(root);
        handler_data.allowed_adapters = vec![CCTP_ADAPTER_PROGRAM_ID];
        handler_data.target_chain_id = 42161;
        handler_data.target_token = REMOTE_USDC;
        let entry = setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &root,
            &USDC_MINT,
            42161,
            REMOTE_USDC,
            6,
        );
        let sra = init_sra_with(svm, &payer, &owner, &root, handler_data.clone(), SALT);
        Self {
            payer,
            owner,
            executor,
            sra,
            root,
            entry,
            handler_data,
        }
    }

    fn source_ix(&self, salt: [u8; 32], finality: u32) -> Instruction {
        let policy = policy_pda(b"source_policy", self.sra, salt);
        Instruction {
            program_id: CCTP_ADAPTER_PROGRAM_ID,
            accounts: sra_cctp_adapter::accounts::InitializeSourcePolicy {
                payer: self.payer.pubkey(),
                owner: self.owner.pubkey(),
                sra_config: self.sra,
                mint: USDC_MINT,
                address_book_root: self.root,
                address_book_entry: self.entry,
                source_policy: policy,
                custody: derive_ata(&policy, &USDC_MINT),
                token_program: anchor_spl::token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: solana_system_interface::program::ID,
            }
            .to_account_metas(None),
            data: sra_cctp_adapter::instruction::InitializeSourcePolicy {
                handler_data: self.handler_data.clone(),
                salt,
                executor: self.executor.pubkey(),
                finality,
                min_gross: 10_000,
                max_gross: 1_000_000,
                max_fee: 100,
            }
            .data(),
        }
    }

    fn update_ix(&self, owner: Pubkey, nonce: u64, paused: bool) -> Instruction {
        Instruction {
            program_id: CCTP_ADAPTER_PROGRAM_ID,
            accounts: sra_cctp_adapter::accounts::SetSourcePolicy {
                owner,
                sra_config: self.sra,
                source_policy: policy_pda(b"source_policy", self.sra, SALT),
            }
            .to_account_metas(None),
            data: sra_cctp_adapter::instruction::SetSourcePolicy {
                expected_nonce: nonce,
                executor: self.executor.pubkey(),
                paused,
            }
            .data(),
        }
    }

    fn destination_ix(&self, route: Pubkey, salt: [u8; 32], finality: u32) -> Instruction {
        let policy = policy_pda(b"destination_policy", route, salt);
        Instruction {
            program_id: CCTP_ADAPTER_PROGRAM_ID,
            accounts: sra_cctp_adapter::accounts::InitializeDestinationPolicy {
                payer: self.payer.pubkey(),
                owner: self.owner.pubkey(),
                handler_route: route,
                mint: USDC_MINT,
                destination_policy: policy,
                custody: derive_ata(&policy, &USDC_MINT),
                execution_lane: derive_execution_lane(&route),
                token_program: anchor_spl::token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: solana_system_interface::program::ID,
                instructions_sysvar: solana_program::sysvar::instructions::ID,
            }
            .to_account_metas(None),
            data: sra_cctp_adapter::instruction::InitializeDestinationPolicy {
                salt,
                sender: evm_bytes32(0x53),
                source_token: REMOTE_USDC,
                finality,
                max_fee: 100,
                min_burn: 10_000,
                max_burn: 1_000_000,
            }
            .data(),
        }
    }
}

fn source_state(svm: &litesvm::LiteSVM, policy: Pubkey) -> SourcePolicy {
    let account = svm.get_account(&policy).unwrap();
    assert_eq!(account.owner, CCTP_ADAPTER_PROGRAM_ID);
    SourcePolicy::try_deserialize(&mut account.data.as_slice()).unwrap()
}

fn assert_custody(svm: &litesvm::LiteSVM, policy: Pubkey) {
    use solana_sdk::program_pack::Pack;
    let account = svm.get_account(&derive_ata(&policy, &USDC_MINT)).unwrap();
    assert_eq!(account.owner, anchor_spl::token::ID);
    let token = anchor_spl::token::spl_token::state::Account::unpack(&account.data).unwrap();
    assert_eq!(token.owner, policy);
    assert_eq!(token.mint, USDC_MINT);
    assert_eq!(token.amount, 0);
}

#[test]
fn managed_policy_source_lifecycle() {
    let mut svm = setup_svm();
    let fx = Fixture::new(&mut svm);
    let policy = policy_pda(b"source_policy", fx.sra, SALT);
    send_ix(&mut svm, fx.source_ix(SALT, 1000), &fx.payer, &[&fx.owner])
        .expect("owner installs paused policy");
    let state = source_state(&svm, policy);
    assert_eq!(state.sra_config, fx.sra);
    assert_eq!(state.mint, USDC_MINT);
    assert_eq!(state.salt, SALT);
    assert_eq!(state.executor, fx.executor.pubkey());
    assert!(state.paused);
    assert_eq!(state.nonce, 0);
    assert_eq!(
        (
            state.finality,
            state.min_gross,
            state.max_gross,
            state.max_fee
        ),
        (1000, 10_000, 1_000_000, 100)
    );
    assert_custody(&svm, policy);
    let second = policy_pda(b"source_policy", fx.sra, [12; 32]);
    send_ix(
        &mut svm,
        fx.source_ix([12; 32], 2000),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap();
    assert_ne!(policy, second);
    assert_custody(&svm, second);
    assert_eq!(source_state(&svm, second).finality, 2000);
    send_ix(
        &mut svm,
        fx.update_ix(fx.owner.pubkey(), 0, false),
        &fx.payer,
        &[&fx.owner],
    )
    .expect("owner unpauses");
    assert_eq!(source_state(&svm, policy).nonce, 1);
    assert!(!source_state(&svm, policy).paused);
    let accounts = |svm: &litesvm::LiteSVM| {
        [
            svm.get_account(&policy).unwrap(),
            svm.get_account(&derive_ata(&policy, &USDC_MINT)).unwrap(),
        ]
    };
    let before = accounts(&svm);
    assert!(send_ix(
        &mut svm,
        fx.update_ix(fx.owner.pubkey(), 0, true),
        &fx.payer,
        &[&fx.owner]
    )
    .is_err());
    assert_eq!(accounts(&svm), before);
    assert!(send_ix(
        &mut svm,
        fx.update_ix(fx.executor.pubkey(), 1, true),
        &fx.payer,
        &[&fx.executor]
    )
    .is_err());
    assert_eq!(accounts(&svm), before);
    for nonce in [1, 2] {
        send_ix(
            &mut svm,
            fx.update_ix(fx.owner.pubkey(), nonce, true),
            &fx.payer,
            &[&fx.owner],
        )
        .unwrap();
        assert_eq!(source_state(&svm, policy).nonce, nonce + 1);
    }
    let before = accounts(&svm);
    assert!(send_ix(&mut svm, fx.source_ix(SALT, 2000), &fx.payer, &[&fx.owner]).is_err());
    assert_eq!(accounts(&svm), before);
    let close = raw_ix(
        "close_source_policy",
        fx.update_ix(fx.owner.pubkey(), 3, true).accounts,
        (),
    );
    assert!(send_ix(&mut svm, close, &fx.payer, &[&fx.owner]).is_err());
    assert_eq!(accounts(&svm), before);
}

#[test]
fn managed_policy_source_setup_authorization_and_finality() {
    for finality in [0, 1, 999, 1001, 1999, 2001, 1000, 2000] {
        let mut svm = setup_svm();
        let fx = Fixture::new(&mut svm);
        let ix = fx.source_ix(SALT, finality);
        assert_eq!(
            send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).is_ok(),
            [1000, 2000].contains(&finality),
            "finality {finality}"
        );
    }
    let mut svm = setup_svm();
    let fx = Fixture::new(&mut svm);
    let mut ix = fx.source_ix(SALT, 1000);
    ix.accounts[1].pubkey = fx.executor.pubkey();
    assert!(send_ix(&mut svm, ix, &fx.payer, &[&fx.executor]).is_err());
    assert!(svm
        .get_account(&policy_pda(b"source_policy", fx.sra, SALT))
        .is_none());
}

#[test]
fn managed_policy_destination_shared_lane_preserves_active_tuple() {
    let mut svm = setup_svm();
    let fx = Fixture::new(&mut svm);
    let mut params = sample_create_route_params(USDC_MINT, SALT);
    params.allowed_adapters = vec![CCTP_ADAPTER_PROGRAM_ID];
    params.allowed_executors = vec![fx.executor.pubkey()];
    let hash = route_config_hash_for(&fx.owner.pubkey(), &params);
    let route = derive_route_pda(&fx.owner.pubkey(), &hash, &SALT).0;
    send_instructions(
        &mut svm,
        &[
            create_route_ix(
                &fx.payer.pubkey(),
                &fx.owner.pubkey(),
                params,
                &[CCTP_ADAPTER_PROGRAM_ID],
            ),
            fx.destination_ix(route, SALT, 1000),
        ],
        &fx.payer,
        &[&fx.owner],
    )
    .expect("atomic fresh route and policy enrollment");
    let first = policy_pda(b"destination_policy", route, SALT);
    assert_custody(&svm, first);
    let account = svm.get_account(&first).unwrap();
    let policy = DestinationPolicy::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(policy.handler_route, route);
    assert_eq!(policy.sender, evm_bytes32(0x53));
    assert_eq!(policy.source_token, REMOTE_USDC);
    assert_eq!(
        (
            policy.source_domain,
            policy.finality,
            policy.queued_liability
        ),
        (3, 1000, 0)
    );
    let lane = derive_execution_lane(&route);
    let mut account = svm.get_account(&lane).unwrap();
    assert_eq!(account.owner, CCTP_ADAPTER_PROGRAM_ID);
    assert_eq!(&account.data[8..40], PROGRAM_ID.as_ref());
    assert_eq!(&account.data[40..72], route.as_ref());
    assert_eq!(account.data[72], 0);
    let mut lane_state = ExecutionLane::try_deserialize(&mut account.data.as_slice()).unwrap();
    lane_state.active = Some(ActiveFill {
        destination_policy: first,
        cctp_receipt: Pubkey::new_unique(),
        handler_receipt: Pubkey::new_unique(),
    });
    lane_state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(lane, account.clone()).unwrap();
    send_ix(
        &mut svm,
        fx.destination_ix(route, [12; 32], 2000),
        &fx.payer,
        &[&fx.owner],
    )
    .expect("later policy loads occupied lane");
    assert_eq!(svm.get_account(&lane).unwrap().data, account.data);
    let second = policy_pda(b"destination_policy", route, [12; 32]);
    assert_ne!(first, second);
    assert_custody(&svm, second);
    assert!(send_ix(
        &mut svm,
        fx.destination_ix(route, SALT, 1000),
        &fx.payer,
        &[&fx.owner]
    )
    .is_err());
}

#[test]
fn managed_policy_source_rejects_substituted_bindings() {
    for case in [
        "sra_owner",
        "sra_pda",
        "mint",
        "decimals",
        "root",
        "entry",
        "custody",
        "policy",
        "handler_data",
        "adapter",
        "corridor",
        "bounds",
        "executor",
    ] {
        let mut svm = setup_svm();
        let mut fx = Fixture::new(&mut svm);
        let mut ix = fx.source_ix(SALT, 1000);
        match case {
            "sra_owner" => {
                let mut account = svm.get_account(&fx.sra).unwrap();
                account.owner = CCTP_ADAPTER_PROGRAM_ID;
                svm.set_account(fx.sra, account).unwrap();
            }
            "sra_pda" => {
                let substitute = Pubkey::new_unique();
                svm.set_account(substitute, svm.get_account(&fx.sra).unwrap())
                    .unwrap();
                ix.accounts[2].pubkey = substitute;
            }
            "mint" => {
                ix.accounts[3].pubkey = create_mint(&mut svm, &fx.payer, &fx.payer.pubkey(), 6);
            }
            "decimals" => {
                let mut account = svm.get_account(&USDC_MINT).unwrap();
                account.data[44] = 9;
                svm.set_account(USDC_MINT, account).unwrap();
            }
            "root" => {
                ix.accounts[4].pubkey = Pubkey::new_unique();
            }
            "entry" => {
                let mut account = svm.get_account(&fx.entry).unwrap();
                account.data[8 + 32 + 32 + 8 + 32] = 0;
                svm.set_account(fx.entry, account).unwrap();
            }
            "custody" => {
                ix.accounts[7].pubkey = Pubkey::new_unique();
            }
            "policy" => {
                ix.accounts[6].pubkey = Pubkey::new_unique();
            }
            "handler_data" => {
                fx.handler_data.recipient = evm_bytes32(0x99);
                ix = fx.source_ix(SALT, 1000);
            }
            "adapter" | "corridor" => {
                if case == "adapter" {
                    fx.handler_data.allowed_adapters.clear();
                } else {
                    fx.handler_data.target_chain_id = 1;
                }
                fx.sra = init_sra_with(
                    &mut svm,
                    &fx.payer,
                    &fx.owner,
                    &fx.root,
                    fx.handler_data.clone(),
                    SALT,
                );
                ix = fx.source_ix(SALT, 1000);
            }
            "bounds" => {
                let end = ix.data.len();
                ix.data[end - 16..end - 8].copy_from_slice(&1u64.to_le_bytes());
            }
            "executor" => {
                let offset = 8 + fx.handler_data.try_to_vec().unwrap().len() + 32;
                ix.data[offset..offset + 32].fill(0);
            }
            _ => unreachable!(),
        }
        assert!(
            send_ix(&mut svm, ix, &fx.payer, &[&fx.owner]).is_err(),
            "{case} must reject"
        );
        assert!(svm
            .get_account(&policy_pda(b"source_policy", fx.sra, SALT))
            .is_none());
    }
}

#[test]
fn managed_policy_destination_requires_fresh_route_and_live_configuration() {
    for case in [
        "preexisting",
        "owner",
        "adapters",
        "executors_empty",
        "executors_two",
        "source_chain",
        "source_token",
        "sender",
        "finality",
        "fee",
        "bounds",
        "lane",
    ] {
        let mut svm = setup_svm();
        let fx = Fixture::new(&mut svm);
        let mut params = sample_create_route_params(USDC_MINT, SALT);
        params.allowed_adapters = vec![CCTP_ADAPTER_PROGRAM_ID];
        params.allowed_executors = vec![fx.executor.pubkey()];
        match case {
            "adapters" => params.allowed_adapters.clear(),
            "executors_empty" => params.allowed_executors.clear(),
            "executors_two" => params.allowed_executors.push(fx.owner.pubkey()),
            "source_chain" => params.source_chain_id = 1,
            _ => {}
        }
        let hash = route_config_hash_for(&fx.owner.pubkey(), &params);
        let route = derive_route_pda(&fx.owner.pubkey(), &hash, &SALT).0;
        let create = create_route_ix(
            &fx.payer.pubkey(),
            &fx.owner.pubkey(),
            params,
            &[CCTP_ADAPTER_PROGRAM_ID],
        );
        let mut setup = fx.destination_ix(route, SALT, 1000);
        match case {
            "owner" => setup.accounts[1].pubkey = fx.executor.pubkey(),
            "source_token" => setup.data[72..104].fill(0),
            "sender" => setup.data[40..72].fill(0),
            "finality" => setup.data[104..108].copy_from_slice(&1u32.to_le_bytes()),
            "fee" => setup.data[108..116].copy_from_slice(&1_000_000u64.to_le_bytes()),
            "bounds" => setup.data[124..132].copy_from_slice(&1u64.to_le_bytes()),
            "lane" => setup.accounts[6].pubkey = Pubkey::new_unique(),
            _ => {}
        }
        let result = if case == "preexisting" {
            send_ix(&mut svm, create, &fx.payer, &[&fx.owner]).unwrap();
            send_ix(&mut svm, setup, &fx.payer, &[&fx.owner])
        } else {
            let signers = if case == "owner" {
                vec![&fx.owner, &fx.executor]
            } else {
                vec![&fx.owner]
            };
            send_instructions(&mut svm, &[create, setup], &fx.payer, &signers).map(|_| ())
        };
        assert!(result.is_err(), "{case} must reject");
        assert!(svm
            .get_account(&policy_pda(b"destination_policy", route, SALT))
            .is_none());
        assert!(svm.get_account(&derive_execution_lane(&route)).is_none());
    }
}

#[test]
fn managed_policy_source_nonce_overflow_preserves_controls() {
    let mut svm = setup_svm();
    let fx = Fixture::new(&mut svm);
    send_ix(&mut svm, fx.source_ix(SALT, 1000), &fx.payer, &[&fx.owner]).unwrap();
    let policy = policy_pda(b"source_policy", fx.sra, SALT);
    let mut account = svm.get_account(&policy).unwrap();
    let mut state = source_state(&svm, policy);
    state.nonce = u64::MAX;
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(policy, account.clone()).unwrap();
    let err = send_ix(
        &mut svm,
        fx.update_ix(fx.owner.pubkey(), u64::MAX, false),
        &fx.payer,
        &[&fx.owner],
    )
    .unwrap_err();
    assert_eq!(
        err.err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            solana_sdk::instruction::InstructionError::Custom(
                sra_cctp_adapter::CctpAdapterError::ArithmeticOverflow.into()
            )
        )
    );
    assert_eq!(svm.get_account(&policy).unwrap().data, account.data);
}
