#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

use anchor_lang::{
    AccountDeserialize, AnchorSerialize, Discriminator, InstructionData, ToAccountMetas,
};
use anchor_spl::{
    associated_token::{get_associated_token_address, ID as ATA_PROGRAM_ID},
    token::ID as TOKEN_PROGRAM_ID,
};
use litesvm::LiteSVM;
use litesvm_token::{
    get_spl_account, spl_token::state::Account as SplTokenAccount, CreateAssociatedTokenAccount,
    CreateMint, MintTo,
};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::program as system_program;
use sra_common::{
    constants::ACROSS_SPOKE_POOL_ID,
    instruction_data::encode_anchor_instruction_data,
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_SRA},
    source_deposit::{AcrossDepositParams, PrepareParams},
    test_utils::DEFAULT_ACTION_IDXS,
    types::{AddressBookEntry, AddressBookRoot, MintNonce, SourceHandlerData, SraConfig},
};
use std::{fs, path::PathBuf};

pub const PROGRAM_ID: Pubkey = sra_handler::ID;
pub const ADAPTER_PROGRAM_ID: Pubkey = sra_across_adapter::ID;
pub const SOL: u64 = 1_000_000_000;

fn program_so_path() -> PathBuf {
    deploy_artifact_path("sra_handler.so")
}

fn adapter_program_so_path() -> PathBuf {
    deploy_artifact_path("sra_across_adapter.so")
}

fn mock_spoke_pool_so_path() -> PathBuf {
    deploy_artifact_path("mock_spoke_pool.so")
}

fn deploy_artifact_path(file_name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let worktree_local = manifest_dir.join(format!("../../target/deploy/{file_name}"));
    if worktree_local.exists() {
        return worktree_local;
    }

    manifest_dir.join(format!("../../../../target/deploy/{file_name}"))
}

pub fn setup_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    let so_path = program_so_path();
    let so = fs::read(&so_path).unwrap_or_else(|err| {
        panic!(
            "failed to read program binary at {}: {err}. Run `anchor build` first.",
            so_path.display()
        )
    });
    svm.add_program(PROGRAM_ID, so.as_slice()).unwrap();

    let adapter_so_path = adapter_program_so_path();
    let adapter_so = fs::read(&adapter_so_path).unwrap_or_else(|err| {
        panic!(
            "failed to read program binary at {}: {err}. Run `anchor build` first.",
            adapter_so_path.display()
        )
    });
    svm.add_program(ADAPTER_PROGRAM_ID, adapter_so.as_slice())
        .unwrap();

    let mock_spoke_so_path = mock_spoke_pool_so_path();
    let mock_spoke_so = fs::read(&mock_spoke_so_path).unwrap_or_else(|err| {
        panic!(
            "failed to read program binary at {}: {err}. Run `anchor build` first.",
            mock_spoke_so_path.display()
        )
    });
    svm.add_program(ACROSS_SPOKE_POOL_ID, mock_spoke_so.as_slice())
        .unwrap();
    svm
}

pub fn funded_keypair(svm: &mut LiteSVM, lamports: u64) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), lamports).unwrap();
    kp
}

pub fn evm_bytes32(byte: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].fill(byte);
    out
}

pub fn sample_target_token() -> [u8; 32] {
    evm_bytes32(0xAB)
}

pub fn sample_handler_data(address_book_root: Pubkey) -> SourceHandlerData {
    SourceHandlerData {
        address_book_root,
        fallback_recipient: evm_bytes32(0xF0),
        allowed_slippage_bps: 50,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
        action_config_hash: [0x44u8; 32],
    }
}

pub fn sample_prepare_params(target_token: [u8; 32]) -> PrepareParams {
    PrepareParams::new(
        1_000_000,
        999_500,
        42_161,
        evm_bytes32(0x11),
        evm_bytes32(0x22),
        evm_bytes32(0x33),
        [0x44u8; 32],
        target_token,
        DEFAULT_ACTION_IDXS.to_vec(),
    )
}

pub fn derive_sra_pda(
    owner: &Pubkey,
    handler_data: &SourceHandlerData,
    salt: &[u8; 32],
) -> (Pubkey, u8) {
    let config_hash = handler_data.config_hash(owner, &PROGRAM_ID);
    Pubkey::find_program_address(&[SEED_SRA, owner.as_ref(), &config_hash, salt], &PROGRAM_ID)
}

pub fn derive_address_book_pda(
    address_book_root: &Pubkey,
    target_chain_id: u64,
    target_token: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_ADDRESS_BOOK,
            address_book_root.as_ref(),
            &target_chain_id.to_le_bytes(),
            target_token,
        ],
        &PROGRAM_ID,
    )
}

pub fn derive_mint_nonce_pda(sra_config: &Pubkey, source_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_MINT_NONCE, sra_config.as_ref(), source_mint.as_ref()],
        &PROGRAM_ID,
    )
}

pub fn derive_adapter_authority_pda(sra_config: &Pubkey, adapter_program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_ADAPTER_AUTHORITY, sra_config.as_ref()],
        adapter_program,
    )
}

pub fn derive_real_across_state_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"state", &0u64.to_le_bytes()], &ACROSS_SPOKE_POOL_ID)
}

pub fn derive_real_across_event_authority_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"__event_authority"], &ACROSS_SPOKE_POOL_ID)
}

pub fn initialize_address_book_root_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    address_book_root: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*address_book_root, true),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: sra_handler::instruction::InitializeAddressBookRoot {}.data(),
    }
}

pub fn initialize_sra_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    address_book_root: &Pubkey,
    sra_config: &Pubkey,
    handler_data: SourceHandlerData,
    salt: [u8; 32],
) -> Instruction {
    let config_hash = handler_data.config_hash(owner, &PROGRAM_ID);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::InitializeSra {
            payer: *payer,
            owner: *owner,
            address_book_root: *address_book_root,
            sra_config: *sra_config,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::InitializeSra {
            config_hash,
            handler_data,
            salt,
        }
        .data(),
    }
}

pub fn set_address_book_entry_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    address_book_root: &Pubkey,
    address_book_entry: &Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetAddressBookEntry {
            payer: *payer,
            owner: *owner,
            address_book_root: *address_book_root,
            address_book_entry: *address_book_entry,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetAddressBookEntry {
            target_chain_id,
            target_token,
            target_decimals,
        }
        .data(),
    }
}

pub fn initialize_address_book_root(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    address_book_root: &Keypair,
) {
    let ix = initialize_address_book_root_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        &address_book_root.pubkey(),
    );
    send_ix(svm, ix, payer, &[owner, address_book_root])
        .expect("initialize_address_book_root should succeed");
}

pub fn init_address_book_root(svm: &mut LiteSVM, payer: &Keypair, owner: &Keypair) -> Pubkey {
    let address_book_root = Keypair::new();
    let pubkey = address_book_root.pubkey();
    initialize_address_book_root(svm, payer, owner, &address_book_root);
    pubkey
}

pub fn init_sra_with(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    address_book_root: &Pubkey,
    handler_data: SourceHandlerData,
    salt: [u8; 32],
) -> Pubkey {
    let (pda, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
    let ix = initialize_sra_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        address_book_root,
        &pda,
        handler_data,
        salt,
    );
    send_ix(svm, ix, payer, &[owner]).expect("initialize_sra should succeed");
    pda
}

pub fn send_ix(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<(), Box<litesvm::types::FailedTransactionMetadata>> {
    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new(&[ix], Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    let mut all_signers: Vec<&Keypair> = vec![payer];
    all_signers.extend_from_slice(signers);
    tx.sign(&all_signers, blockhash);
    svm.send_transaction(tx).map(|_| ()).map_err(Box::new)
}

pub fn send_ix_with_meta(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    send_instructions(svm, &[ix], payer, signers)
}

pub fn send_instructions(
    svm: &mut LiteSVM,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    svm.expire_blockhash();
    let blockhash = svm.latest_blockhash();
    let msg = Message::new(ixs, Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    let mut all_signers: Vec<&Keypair> = vec![payer];
    all_signers.extend_from_slice(signers);
    tx.sign(&all_signers, blockhash);
    svm.send_transaction(tx).map_err(Box::new)
}

pub fn send_ix_expect_err(
    svm: &mut LiteSVM,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
    msg: &str,
) -> TransactionError {
    send_ix(svm, ix, payer, signers).expect_err(msg).err.clone()
}

pub fn fetch_sra_config(svm: &LiteSVM, pda: &Pubkey) -> SraConfig {
    let acct = svm.get_account(pda).expect("sra_config account exists");
    SraConfig::try_deserialize(&mut acct.data.as_slice()).expect("valid SraConfig")
}

pub fn fetch_address_book_root(svm: &LiteSVM, address: &Pubkey) -> AddressBookRoot {
    let acct = svm
        .get_account(address)
        .expect("address_book_root account exists");
    AddressBookRoot::try_deserialize(&mut acct.data.as_slice()).expect("valid AddressBookRoot")
}

pub fn fetch_address_book_entry(svm: &LiteSVM, pda: &Pubkey) -> AddressBookEntry {
    let acct = svm
        .get_account(pda)
        .expect("address_book_entry account exists");
    AddressBookEntry::try_deserialize(&mut acct.data.as_slice()).expect("valid AddressBookEntry")
}

pub fn fetch_mint_nonce(svm: &LiteSVM, pda: &Pubkey) -> MintNonce {
    let acct = svm.get_account(pda).expect("mint_nonce account exists");
    MintNonce::try_deserialize(&mut acct.data.as_slice()).expect("valid MintNonce")
}

pub fn seed_mint_nonce_account(svm: &mut LiteSVM, pda: &Pubkey, nonce: u64, bump: u8) {
    let mut data = <MintNonce as Discriminator>::DISCRIMINATOR.to_vec();
    MintNonce { nonce, bump }.serialize(&mut data).unwrap();

    let account = solana_sdk::account::Account {
        lamports: svm.minimum_balance_for_rent_exemption(data.len()),
        data,
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    svm.set_account(*pda, account).unwrap();
}

pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(owner, mint)
}

pub fn create_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint_authority: &Pubkey,
    decimals: u8,
) -> Pubkey {
    CreateMint::new(svm, payer)
        .authority(mint_authority)
        .decimals(decimals)
        .send()
        .expect("create_mint should succeed")
}

pub fn create_ata(svm: &mut LiteSVM, payer: &Keypair, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    CreateAssociatedTokenAccount::new(svm, payer, mint)
        .owner(owner)
        .send()
        .expect("create_associated_token_account should succeed")
}

pub fn mint_to(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint_authority: &Keypair,
    mint: &Pubkey,
    to: &Pubkey,
    amount: u64,
) {
    MintTo::new(svm, payer, mint, to, amount)
        .owner(mint_authority)
        .send()
        .expect("mint_to should succeed");
}

pub fn fetch_token_balance(svm: &LiteSVM, ata: &Pubkey) -> u64 {
    get_spl_account::<SplTokenAccount>(svm, ata)
        .expect("token account exists")
        .amount
}

pub fn setup_address_book_entry(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    address_book_root: &Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Pubkey {
    let (pda, _) = derive_address_book_pda(address_book_root, target_chain_id, &target_token);
    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        address_book_root,
        &pda,
        target_chain_id,
        target_token,
        target_decimals,
    );
    send_ix(svm, ix, payer, &[owner]).expect("set_address_book_entry should succeed");
    pda
}

#[derive(Clone)]
pub struct ValidatePrepareAccounts {
    pub sra_config: Pubkey,
    pub mint_nonce: Pubkey,
    pub source_mint: Pubkey,
    pub sra_vault: Pubkey,
    pub adapter_program: Pubkey,
    pub adapter_authority: Pubkey,
    pub adapter_custody: Pubkey,
}

impl ValidatePrepareAccounts {
    pub fn derive(sra_config: &Pubkey, source_mint: &Pubkey, adapter_program: &Pubkey) -> Self {
        let (mint_nonce, _) = derive_mint_nonce_pda(sra_config, source_mint);
        let (adapter_authority, _) = derive_adapter_authority_pda(sra_config, adapter_program);
        Self {
            sra_config: *sra_config,
            mint_nonce,
            source_mint: *source_mint,
            sra_vault: derive_ata(sra_config, source_mint),
            adapter_program: *adapter_program,
            adapter_authority,
            adapter_custody: derive_ata(&adapter_authority, source_mint),
        }
    }
}

pub fn validate_and_prepare_ix(
    fee_payer: &Pubkey,
    owner: &Pubkey,
    accounts: &ValidatePrepareAccounts,
    owner_token_account: &Pubkey,
    address_book_root: &Pubkey,
    address_book_entry: &Pubkey,
    handler_data: SourceHandlerData,
    params: PrepareParams,
    paired_execute_ix: &Instruction,
) -> Instruction {
    let paired_adapter_ix_data_hash =
        sra_common::instruction_data::instruction_data_hash(&paired_execute_ix.data);

    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ValidateAndPrepare {
            fee_payer: *fee_payer,
            owner: *owner,
            sra_config: accounts.sra_config,
            mint_nonce: accounts.mint_nonce,
            source_mint: accounts.source_mint,
            owner_token_account: *owner_token_account,
            sra_vault: accounts.sra_vault,
            adapter_authority: accounts.adapter_authority,
            adapter_custody: accounts.adapter_custody,
            adapter_program: accounts.adapter_program,
            adapter_program_data: accounts.adapter_program,
            address_book_root: *address_book_root,
            address_book_entry: *address_book_entry,
            token_program: TOKEN_PROGRAM_ID,
            associated_token_program: ATA_PROGRAM_ID,
            system_program: system_program::ID,
            instructions_sysvar: anchor_lang::solana_program::sysvar::instructions::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ValidateAndPrepare {
            handler_data,
            params,
            paired_adapter_ix_data_hash,
        }
        .data(),
    }
}

pub const OWNER_STARTING_BALANCE: u64 = 10_000_000;

pub struct ValidatePrepareFixture {
    pub payer: Keypair,
    pub owner: Keypair,
    pub address_book_root: Pubkey,
    pub handler_data: SourceHandlerData,
    pub sra_config: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub adapter_program: Pubkey,
    pub accounts: ValidatePrepareAccounts,
    pub owner_ata: Pubkey,
    pub across_state: Pubkey,
    pub across_vault: Pubkey,
}

impl ValidatePrepareFixture {
    pub fn new(svm: &mut LiteSVM) -> Self {
        Self::new_with_adapter_policy(svm, ADAPTER_PROGRAM_ID, 50, vec![ADAPTER_PROGRAM_ID])
    }

    pub fn new_with(svm: &mut LiteSVM, allowed_slippage_bps: u16) -> Self {
        Self::new_with_adapter_policy(
            svm,
            ADAPTER_PROGRAM_ID,
            allowed_slippage_bps,
            vec![ADAPTER_PROGRAM_ID],
        )
    }

    pub fn new_with_adapter_policy(
        svm: &mut LiteSVM,
        adapter_program: Pubkey,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
    ) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);

        let address_book_root = init_address_book_root(svm, &payer, &owner);
        let handler_data = SourceHandlerData {
            address_book_root,
            fallback_recipient: evm_bytes32(0xF0),
            allowed_slippage_bps,
            allowed_adapters,
            action_config_hash: [0x55u8; 32],
        };
        let salt = [0xABu8; 32];
        let sra_config = init_sra_with(
            svm,
            &payer,
            &owner,
            &address_book_root,
            handler_data.clone(),
            salt,
        );

        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        let target_chain_id = 42_161;
        let target_token = sample_target_token();
        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &address_book_root,
            target_chain_id,
            target_token,
            6,
        );

        let accounts = ValidatePrepareAccounts::derive(&sra_config, &source_mint, &adapter_program);
        let owner_ata = create_ata(svm, &payer, &owner.pubkey(), &source_mint);
        let across_state = funded_keypair(svm, SOL).pubkey();
        let across_vault = create_ata(svm, &payer, &across_state, &source_mint);
        mint_to(
            svm,
            &payer,
            &payer,
            &source_mint,
            &owner_ata,
            OWNER_STARTING_BALANCE,
        );

        Self {
            payer,
            owner,
            address_book_root,
            handler_data,
            sra_config,
            source_mint,
            target_chain_id,
            target_token,
            adapter_program,
            accounts,
            owner_ata,
            across_state,
            across_vault,
        }
    }

    pub fn address_book_entry(&self, target_chain_id: u64, target_token: &[u8; 32]) -> Pubkey {
        derive_address_book_pda(&self.address_book_root, target_chain_id, target_token).0
    }

    pub fn invoke(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_with_handler_data(svm, self.handler_data.clone(), params, vec![])
    }

    pub fn invoke_with(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_with_handler_data(svm, self.handler_data.clone(), params, between_ixs)
    }

    pub fn invoke_with_handler_data(
        &self,
        svm: &mut LiteSVM,
        handler_data: SourceHandlerData,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        let execute_ix = execute_deposit_across_ix(
            &self.payer.pubkey(),
            self,
            &fetch_sra_config(svm, &self.sra_config),
            &params,
            across_params_for(self, &params),
            current_nonce(svm, &self.accounts.mint_nonce),
        );
        let address_book_entry =
            self.address_book_entry(params.target_chain_id, &params.target_token);
        let validate_ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &self.address_book_root,
            &address_book_entry,
            handler_data,
            params.clone(),
            &execute_ix,
        );

        let mut ixs = vec![validate_ix];
        ixs.extend(between_ixs);
        ixs.push(execute_ix);
        send_instructions(svm, &ixs, &self.payer, &[&self.owner])
    }
}

pub fn across_params_for(
    fx: &ValidatePrepareFixture,
    params: &PrepareParams,
) -> AcrossDepositParams {
    AcrossDepositParams::from_prepare(params, fx.payer.pubkey(), 3_600, 900)
}

pub fn execute_deposit_across_ix(
    fee_payer: &Pubkey,
    fx: &ValidatePrepareFixture,
    sra_config: &SraConfig,
    prepare: &PrepareParams,
    params: AcrossDepositParams,
    prepared_nonce: u64,
) -> Instruction {
    let bundle = sra_across_adapter::utils::build_deposit_now_bundle(
        &fx.accounts.adapter_authority,
        sra_config,
        &fx.source_mint,
        prepare,
        &params,
        prepared_nonce,
    )
    .expect("build deposit-now bundle");

    Instruction {
        program_id: ADAPTER_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*fee_payer, true),
            AccountMeta::new_readonly(anchor_lang::solana_program::sysvar::instructions::ID, false),
            AccountMeta::new_readonly(fx.sra_config, false),
            AccountMeta::new_readonly(fx.accounts.mint_nonce, false),
            AccountMeta::new_readonly(fx.accounts.adapter_authority, false),
            AccountMeta::new(fx.accounts.adapter_custody, false),
            AccountMeta::new_readonly(fx.source_mint, false),
            AccountMeta::new_readonly(ACROSS_SPOKE_POOL_ID, false),
            AccountMeta::new(fx.across_state, false),
            AccountMeta::new(fx.across_vault, false),
            AccountMeta::new_readonly(derive_real_across_event_authority_pda().0, false),
            AccountMeta::new_readonly(bundle.delegate.pda, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: encode_anchor_instruction_data("execute_deposit_across", &params),
    }
}

pub fn compute_budget_ix() -> Instruction {
    ComputeBudgetInstruction::set_compute_unit_limit(400_000)
}

pub fn invoke_execute_only(
    fx: &ValidatePrepareFixture,
    svm: &mut LiteSVM,
    prepare: &PrepareParams,
    params: AcrossDepositParams,
) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>> {
    let ix = execute_deposit_across_ix(
        &fx.payer.pubkey(),
        fx,
        &fetch_sra_config(svm, &fx.sra_config),
        prepare,
        params,
        current_nonce(svm, &fx.accounts.mint_nonce),
    );
    send_instructions(svm, &[ix], &fx.payer, &[])
}

pub fn current_nonce(svm: &LiteSVM, mint_nonce: &Pubkey) -> u64 {
    svm.get_account(mint_nonce)
        .map(|acct| {
            MintNonce::try_deserialize(&mut acct.data.as_slice())
                .expect("valid MintNonce")
                .nonce
        })
        .unwrap_or(0)
}

pub fn extract_anchor_event<T>(logs: &[String]) -> Option<T>
where
    T: anchor_lang::AnchorDeserialize + anchor_lang::Discriminator,
{
    use anchor_lang::{AnchorDeserialize, Discriminator};
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let expected_disc: &[u8] = <T as Discriminator>::DISCRIMINATOR;

    for log in logs {
        let Some(b64) = log.strip_prefix("Program data: ") else {
            continue;
        };
        let Ok(raw) = engine.decode(b64) else {
            continue;
        };
        if raw.len() < expected_disc.len() || &raw[..expected_disc.len()] != expected_disc {
            continue;
        }
        if let Ok(ev) = <T as AnchorDeserialize>::try_from_slice(&raw[expected_disc.len()..]) {
            return Some(ev);
        }
    }
    None
}

pub fn extract_deposit_prepared(logs: &[String]) -> Option<sra_handler::events::DepositPrepared> {
    extract_anchor_event(logs)
}
