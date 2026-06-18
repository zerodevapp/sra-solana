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
    message::{AddressLookupTableAccount, Message},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::program as system_program;
use sra_across_adapter::utils::svm_spoke::types::RelayData;
use sra_common::{
    abi::u256_from_u64,
    constants::{ACROSS_SPOKE_POOL_ID, SOLANA_CHAIN_ID},
    instruction_data::encode_anchor_instruction_data,
    seeds::{
        action_config_hash, route_config_hash, SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK,
        SEED_ASSET_FEE_TIER, SEED_EXECUTION, SEED_FEE_CONFIG, SEED_FILL_RECEIPT, SEED_MINT_NONCE,
        SEED_PROTOCOL_FEE, SEED_SRA, SEED_SRA_ROUTE, SEED_VERIFIED_FILL,
    },
    source_deposit::{AcrossDepositParams, PrepareParams},
    test_utils::DEFAULT_ACTION_IDXS,
    types::{
        AddressBookEntry, AddressBookRoot, AssetFeeTier, CreateRouteParams, ExecutionStatus,
        FeeConfig, FillReceipt, ManageRouteParams, MintNonce, ProtocolFeeConfig, SolanaAction,
        SolanaSraRoute, SourceHandlerData, SraConfig,
    },
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
        target_chain_id: 42_161,
        target_handler: evm_bytes32(0x11),
        target_adapter: evm_bytes32(0x22),
        recipient: evm_bytes32(0x33),
        app_id: [0x44u8; 32],
        target_token: sample_target_token(),
        allowed_slippage_bps: 50,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
    }
}

pub fn sample_prepare_params(target_token: [u8; 32]) -> PrepareParams {
    PrepareParams::new(
        1_000_000,
        u256_from_u64(999_500),
        42_161,
        evm_bytes32(0x11),
        evm_bytes32(0x22),
        evm_bytes32(0x33),
        [0x44u8; 32],
        target_token,
        DEFAULT_ACTION_IDXS.to_vec(),
        0,
        false,
        sra_common::constants::NO_FEE_BPS_OVERRIDE,
    )
}

pub fn representative_lookup_table(ixs: &[Instruction]) -> AddressLookupTableAccount {
    let mut addresses = Vec::new();

    for ix in ixs {
        for meta in &ix.accounts {
            if meta.is_signer || addresses.contains(&meta.pubkey) {
                continue;
            }
            addresses.push(meta.pubkey);
        }
    }

    AddressLookupTableAccount {
        key: Pubkey::new_unique(),
        addresses,
    }
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
    source_mint: &Pubkey,
    target_chain_id: u64,
    target_token: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            SEED_ADDRESS_BOOK,
            address_book_root.as_ref(),
            source_mint.as_ref(),
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

// --- Mock Across SpokePool: destination fill-status helpers ---
//
// The mock is loaded at the real Across program ID, so a `fill_status` PDA it
// creates is owned by `ACROSS_SPOKE_POOL_ID` and seeded exactly like the real
// program. Destination-fill tests use this to stand up `FillStatusAccount`s the
// adapter can read without a live Across deployment.

pub fn derive_fill_status_pda(relay_hash: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[mock_spoke_pool::FILL_SEED_PREFIX, relay_hash],
        &ACROSS_SPOKE_POOL_ID,
    )
}

/// Realistic relayer fill: moves `output_amount` to the recipient and writes a
/// `Filled` `FillStatusAccount` at `["fills", relay_hash]`.
pub fn mock_fill_relay_ix(
    relayer: &Pubkey,
    mint: &Pubkey,
    relayer_token_account: &Pubkey,
    recipient_token_account: &Pubkey,
    relay_hash: [u8; 32],
    output_amount: u64,
    fill_deadline: u32,
) -> Instruction {
    let (fill_status, _) = derive_fill_status_pda(&relay_hash);
    Instruction {
        program_id: ACROSS_SPOKE_POOL_ID,
        accounts: mock_spoke_pool::accounts::FillRelay {
            signer: *relayer,
            mint: *mint,
            relayer_token_account: *relayer_token_account,
            recipient_token_account: *recipient_token_account,
            fill_status,
            token_program: TOKEN_PROGRAM_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: mock_spoke_pool::instruction::FillRelay {
            relay_hash,
            output_amount,
            fill_deadline,
        }
        .data(),
    }
}

/// Test backdoor: plant a `FillStatusAccount` with an arbitrary status (e.g.
/// `Unfilled`) so the adapter's reject paths can be exercised.
pub fn mock_fill_ix(
    relayer: &Pubkey,
    relay_hash: [u8; 32],
    status: mock_spoke_pool::FillStatus,
) -> Instruction {
    let (fill_status, _) = derive_fill_status_pda(&relay_hash);
    Instruction {
        program_id: ACROSS_SPOKE_POOL_ID,
        accounts: mock_spoke_pool::accounts::MockFill {
            signer: *relayer,
            fill_status,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: mock_spoke_pool::instruction::MockFill { relay_hash, status }.data(),
    }
}

/// Test backdoor: the relayer reclaims a `FillStatusAccount`'s rent (the
/// closure race the adapter must survive).
pub fn close_fill_pda_ix(relayer: &Pubkey, fill_status: &Pubkey) -> Instruction {
    Instruction {
        program_id: ACROSS_SPOKE_POOL_ID,
        accounts: mock_spoke_pool::accounts::CloseFillPda {
            signer: *relayer,
            fill_status: *fill_status,
        }
        .to_account_metas(None),
        data: mock_spoke_pool::instruction::CloseFillPda {}.data(),
    }
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
    source_mint: Pubkey,
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
            source_mint,
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

// --- Fee admin (shared; will be consumed by the source deposit path) ---

pub fn derive_fee_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_FEE_CONFIG], &PROGRAM_ID)
}

pub fn derive_protocol_fee_pda(app_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_PROTOCOL_FEE, app_id], &PROGRAM_ID)
}

pub fn derive_asset_fee_tier_pda(app_id: &[u8; 32], mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_ASSET_FEE_TIER, app_id, mint.as_ref()], &PROGRAM_ID)
}

pub fn initialize_fee_config_ix(
    payer: &Pubkey,
    admin: &Pubkey,
    fee_recipient: Pubkey,
) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::InitializeFeeConfig {
            payer: *payer,
            admin: *admin,
            fee_config,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::InitializeFeeConfig { fee_recipient }.data(),
    }
}

pub fn set_fee_recipient_ix(admin: &Pubkey, new_recipient: Pubkey) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetFeeRecipient {
            admin: *admin,
            fee_config,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetFeeRecipient { new_recipient }.data(),
    }
}

pub fn set_protocol_fee_ix(
    payer: &Pubkey,
    admin: &Pubkey,
    app_id: [u8; 32],
    default_fee_bps: u16,
    allow_override: bool,
) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    let (protocol_fee_config, _) = derive_protocol_fee_pda(&app_id);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetProtocolFee {
            payer: *payer,
            admin: *admin,
            fee_config,
            protocol_fee_config,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetProtocolFee {
            app_id,
            default_fee_bps,
            allow_override,
        }
        .data(),
    }
}

pub fn remove_protocol_fee_ix(admin: &Pubkey, app_id: [u8; 32]) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    let (protocol_fee_config, _) = derive_protocol_fee_pda(&app_id);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::RemoveProtocolFee {
            admin: *admin,
            fee_config,
            protocol_fee_config,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::RemoveProtocolFee { app_id }.data(),
    }
}

pub fn set_asset_fee_tier_ix(
    payer: &Pubkey,
    admin: &Pubkey,
    app_id: [u8; 32],
    mint: Pubkey,
    threshold: u64,
    below_bps: u16,
    above_or_equal_bps: u16,
) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    let (protocol_fee_config, _) = derive_protocol_fee_pda(&app_id);
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&app_id, &mint);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetAssetFeeTier {
            payer: *payer,
            admin: *admin,
            fee_config,
            protocol_fee_config,
            asset_fee_tier,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetAssetFeeTier {
            app_id,
            mint,
            threshold,
            below_bps,
            above_or_equal_bps,
        }
        .data(),
    }
}

pub fn remove_asset_fee_tier_ix(admin: &Pubkey, app_id: [u8; 32], mint: Pubkey) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    let (asset_fee_tier, _) = derive_asset_fee_tier_pda(&app_id, &mint);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::RemoveAssetFeeTier {
            admin: *admin,
            fee_config,
            asset_fee_tier,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::RemoveAssetFeeTier { app_id, mint }.data(),
    }
}

pub fn set_pending_admin_ix(admin: &Pubkey, new_pending_admin: Pubkey) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::SetPendingAdmin {
            admin: *admin,
            fee_config,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::SetPendingAdmin { new_pending_admin }.data(),
    }
}

pub fn accept_admin_ix(pending_admin: &Pubkey) -> Instruction {
    let (fee_config, _) = derive_fee_config_pda();
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::AcceptAdmin {
            pending_admin: *pending_admin,
            fee_config,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::AcceptAdmin {}.data(),
    }
}

pub fn fetch_fee_config(svm: &LiteSVM, pda: &Pubkey) -> FeeConfig {
    let acct = svm.get_account(pda).expect("fee_config account exists");
    FeeConfig::try_deserialize(&mut acct.data.as_slice()).expect("valid FeeConfig")
}

pub fn fetch_protocol_fee_config(svm: &LiteSVM, pda: &Pubkey) -> ProtocolFeeConfig {
    let acct = svm
        .get_account(pda)
        .expect("protocol_fee_config account exists");
    ProtocolFeeConfig::try_deserialize(&mut acct.data.as_slice()).expect("valid ProtocolFeeConfig")
}

pub fn fetch_asset_fee_tier(svm: &LiteSVM, pda: &Pubkey) -> AssetFeeTier {
    let acct = svm.get_account(pda).expect("asset_fee_tier account exists");
    AssetFeeTier::try_deserialize(&mut acct.data.as_slice()).expect("valid AssetFeeTier")
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

pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn restamp_mint_as_token_2022(svm: &mut LiteSVM, mint: &Pubkey) {
    let mut account = svm.get_account(mint).expect("mint account exists");
    account.owner = TOKEN_2022_PROGRAM_ID;
    svm.set_account(*mint, account)
        .expect("restamp mint owner should succeed");
}

pub fn freeze_token_account(svm: &mut LiteSVM, ata: &Pubkey) {
    use litesvm_token::spl_token::state::AccountState;
    use solana_sdk::program_pack::Pack;

    let mut account = svm.get_account(ata).expect("token account exists");
    let mut token = SplTokenAccount::unpack(&account.data).expect("initialized SPL token account");
    token.state = AccountState::Frozen;
    token.pack_into_slice(&mut account.data);
    svm.set_account(*ata, account)
        .expect("freeze token account should succeed");
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
    source_mint: &Pubkey,
    target_chain_id: u64,
    target_token: [u8; 32],
    target_decimals: u8,
) -> Pubkey {
    let (pda, _) = derive_address_book_pda(
        address_book_root,
        source_mint,
        target_chain_id,
        &target_token,
    );
    let ix = set_address_book_entry_ix(
        &payer.pubkey(),
        &owner.pubkey(),
        address_book_root,
        &pda,
        *source_mint,
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

#[allow(clippy::too_many_arguments)]
pub fn validate_and_prepare_ix(
    fee_payer: &Pubkey,
    owner: &Pubkey,
    accounts: &ValidatePrepareAccounts,
    owner_token_account: &Pubkey,
    address_book_root: &Pubkey,
    address_book_entry: &Pubkey,
    fee_recipient_token_account: Option<Pubkey>,
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
            address_book_root: *address_book_root,
            address_book_entry: *address_book_entry,
            fee_config: derive_fee_config_pda().0,
            protocol_fee_config: derive_protocol_fee_pda(&handler_data.app_id).0,
            asset_fee_tier: derive_asset_fee_tier_pda(&handler_data.app_id, &accounts.source_mint)
                .0,
            fee_recipient_token_account,
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
    /// Admin of the singleton fee config. The fixture initializes the config
    /// with `Pubkey::default()` recipient (fees globally disabled) so the
    /// zero-fee deposit paths run against genesis-like fee state; fee tests
    /// use this admin to enable fees.
    pub fee_admin: Keypair,
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
        Self::new_with_decimals_and_adapter_policy(
            svm,
            6,
            6,
            ADAPTER_PROGRAM_ID,
            50,
            vec![ADAPTER_PROGRAM_ID],
        )
    }

    pub fn new_with(svm: &mut LiteSVM, allowed_slippage_bps: u16) -> Self {
        Self::new_with_decimals_and_adapter_policy(
            svm,
            6,
            6,
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
        Self::new_with_decimals_and_adapter_policy(
            svm,
            6,
            6,
            adapter_program,
            allowed_slippage_bps,
            allowed_adapters,
        )
    }

    pub fn new_with_decimals_and_adapter_policy(
        svm: &mut LiteSVM,
        source_decimals: u8,
        target_decimals: u8,
        adapter_program: Pubkey,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
    ) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let owner = funded_keypair(svm, SOL);

        let fee_admin = funded_keypair(svm, SOL);
        if svm.get_account(&derive_fee_config_pda().0).is_none() {
            send_instructions(
                svm,
                &[initialize_fee_config_ix(
                    &payer.pubkey(),
                    &fee_admin.pubkey(),
                    Pubkey::default(),
                )],
                &payer,
                &[&fee_admin],
            )
            .expect("fee config init must succeed");
        }

        let address_book_root = init_address_book_root(svm, &payer, &owner);
        let target_chain_id = 42_161;
        let target_token = sample_target_token();
        let handler_data = SourceHandlerData {
            address_book_root,
            target_chain_id,
            target_handler: evm_bytes32(0x11),
            target_adapter: evm_bytes32(0x22),
            recipient: evm_bytes32(0x33),
            app_id: [0x44u8; 32],
            target_token,
            allowed_slippage_bps,
            allowed_adapters,
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

        let source_mint = create_mint(svm, &payer, &payer.pubkey(), source_decimals);
        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &address_book_root,
            &source_mint,
            target_chain_id,
            target_token,
            target_decimals,
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
            fee_admin,
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
        self.address_book_entry_for(&self.source_mint, target_chain_id, target_token)
    }

    pub fn address_book_entry_for(
        &self,
        source_mint: &Pubkey,
        target_chain_id: u64,
        target_token: &[u8; 32],
    ) -> Pubkey {
        derive_address_book_pda(
            &self.address_book_root,
            source_mint,
            target_chain_id,
            target_token,
        )
        .0
    }

    pub fn invoke(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(svm, self.handler_data.clone(), params, vec![], None, None)
    }

    pub fn invoke_with(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(
            svm,
            self.handler_data.clone(),
            params,
            between_ixs,
            None,
            None,
        )
    }

    pub fn invoke_with_handler_data(
        &self,
        svm: &mut LiteSVM,
        handler_data: SourceHandlerData,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(svm, handler_data, params, between_ixs, None, None)
    }

    pub fn invoke_with_fee_ata(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        fee_recipient_token_account: Option<Pubkey>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(
            svm,
            self.handler_data.clone(),
            params,
            vec![],
            fee_recipient_token_account,
            None,
        )
    }

    /// Builds the normal 2-IX flow, then replaces the account-meta holding
    /// `victim` with `substitute` in the validate instruction — substitution
    /// suites pin that every fee account is constraint-bound, never
    /// caller-chosen.
    pub fn invoke_with_fee_accounts_mutation(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        fee_recipient_token_account: Option<Pubkey>,
        victim: Pubkey,
        substitute: Pubkey,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(
            svm,
            self.handler_data.clone(),
            params,
            vec![],
            fee_recipient_token_account,
            Some((victim, substitute)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_full(
        &self,
        svm: &mut LiteSVM,
        handler_data: SourceHandlerData,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
        fee_recipient_token_account: Option<Pubkey>,
        validate_meta_substitution: Option<(Pubkey, Pubkey)>,
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
        let mut validate_ix = validate_and_prepare_ix(
            &self.payer.pubkey(),
            &self.owner.pubkey(),
            &self.accounts,
            &self.owner_ata,
            &self.address_book_root,
            &address_book_entry,
            fee_recipient_token_account,
            handler_data,
            params.clone(),
            &execute_ix,
        );
        if let Some((victim, substitute)) = validate_meta_substitution {
            let slot = validate_ix
                .accounts
                .iter()
                .position(|meta| meta.pubkey == victim)
                .expect("victim account must be present in the validate instruction");
            validate_ix.accounts[slot].pubkey = substitute;
        }

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
        .expect("prepare params must produce a valid net amount")
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

pub fn recover_stuck_custody_ix(
    fee_payer: &Pubkey,
    owner: &Pubkey,
    sra_config: &Pubkey,
    adapter_authority: &Pubkey,
    mint: &Pubkey,
    adapter_custody: &Pubkey,
    owner_token_account: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: ADAPTER_PROGRAM_ID,
        accounts: sra_across_adapter::accounts::RecoverStuckCustody {
            fee_payer: *fee_payer,
            owner: *owner,
            sra_config: *sra_config,
            adapter_authority: *adapter_authority,
            mint: *mint,
            adapter_custody: *adapter_custody,
            owner_token_account: *owner_token_account,
            token_program: TOKEN_PROGRAM_ID,
        }
        .to_account_metas(None),
        data: sra_across_adapter::instruction::RecoverStuckCustody {}.data(),
    }
}

/// The compute-unit budget the production source deposit tx requests. The full
/// 2-IX flow must execute under this — `compute_budget_ix` requests exactly this
/// much, and `tests/source_cu_budget.rs` asserts consumption stays below it.
/// Single source of truth so the requested limit and the regression gate can't
/// drift. Typed `u32` to match the runtime's compute-unit-limit field (max
/// 1.4M) — no narrowing cast at the call site; the gate widens losslessly to
/// compare against `u64` CU.
pub const SOURCE_CU_BUDGET: u32 = 400_000;

pub fn compute_budget_ix() -> Instruction {
    ComputeBudgetInstruction::set_compute_unit_limit(SOURCE_CU_BUDGET)
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

// --- Destination route admin (create_route) ---

pub fn derive_route_pda(
    owner: &Pubkey,
    route_config_hash: &[u8; 32],
    salt: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_SRA_ROUTE, owner.as_ref(), route_config_hash, salt],
        &PROGRAM_ID,
    )
}

pub fn derive_execution_authority_pda(route: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_EXECUTION, route.as_ref()], &PROGRAM_ID)
}

/// Recompute the route's config hash off-chain (mirrors the on-chain seed).
pub fn route_config_hash_for(owner: &Pubkey, params: &CreateRouteParams) -> [u8; 32] {
    route_config_hash(
        owner,
        params.source_chain_id,
        &params.expected_mint,
        &params.app_id,
        &params.action_config,
    )
    .expect("route_config_hash should not exceed the action bound")
}

pub fn action_config_hash_for(params: &CreateRouteParams) -> [u8; 32] {
    action_config_hash(&params.action_config).expect("action_config_hash within bound")
}

/// Build a `create_route` instruction. `route_config_hash` is supplied explicitly
/// so negative tests can pass a tampered hash; `adapter_accounts` are appended as
/// `remaining_accounts` (the program loader-owned-executable-checks each).
pub fn create_route_ix_with_hash(
    payer: &Pubkey,
    owner: &Pubkey,
    route_config_hash: [u8; 32],
    params: CreateRouteParams,
    adapter_accounts: &[Pubkey],
) -> Instruction {
    let (route, _) = derive_route_pda(owner, &route_config_hash, &params.salt);
    let (execution_authority, _) = derive_execution_authority_pda(&route);
    let route_vault = derive_ata(&route, &params.expected_mint);
    let execution_vault = derive_ata(&execution_authority, &params.expected_mint);

    let mut accounts = sra_handler::accounts::CreateRoute {
        payer: *payer,
        owner: *owner,
        route,
        expected_mint: params.expected_mint,
        route_vault,
        execution_authority,
        execution_vault,
        token_program: TOKEN_PROGRAM_ID,
        associated_token_program: ATA_PROGRAM_ID,
        system_program: system_program::ID,
    }
    .to_account_metas(None);
    for adapter in adapter_accounts {
        accounts.push(AccountMeta::new_readonly(*adapter, false));
    }

    Instruction {
        program_id: PROGRAM_ID,
        accounts,
        data: sra_handler::instruction::CreateRoute {
            route_config_hash,
            params,
        }
        .data(),
    }
}

/// Build a `create_route` instruction with the correctly-derived config hash.
pub fn create_route_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    params: CreateRouteParams,
    adapter_accounts: &[Pubkey],
) -> Instruction {
    let hash = route_config_hash_for(owner, &params);
    create_route_ix_with_hash(payer, owner, hash, params, adapter_accounts)
}

pub fn fetch_route(svm: &LiteSVM, pda: &Pubkey) -> SolanaSraRoute {
    let acct = svm.get_account(pda).expect("route account exists");
    SolanaSraRoute::try_deserialize(&mut acct.data.as_slice()).expect("valid SolanaSraRoute")
}

/// A minimal valid route: one SplTransfer of 100% and the real adapter program.
pub fn sample_create_route_params(expected_mint: Pubkey, salt: [u8; 32]) -> CreateRouteParams {
    CreateRouteParams {
        source_chain_id: 42_161,
        expected_mint,
        app_id: [0x44u8; 32],
        action_config: vec![SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps: 10_000,
        }],
        salt,
        allowed_adapters: vec![ADAPTER_PROGRAM_ID],
    }
}

/// Create the sample route (one allowed adapter, not paused) and return its PDA
/// plus its `route_config_hash` — the canonical route creator. The hash is the
/// signer seed the route needs to authorize vault transfers (e.g. finalize_fill)
/// and can only be captured at creation time, since the sample params carry a
/// freshly-generated recipient that the route can't recompute later.
pub fn create_sample_route_with_hash(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    salt: [u8; 32],
) -> (Pubkey, [u8; 32]) {
    let params = sample_create_route_params(mint, salt);
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    let ix = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        hash,
        params,
        &[ADAPTER_PROGRAM_ID],
    );
    send_ix(svm, ix, payer, &[owner]).expect("create_route should succeed");
    (route_pda, hash)
}

/// Create the sample route (one allowed adapter, not paused) and return its PDA.
/// Thin wrapper over [`create_sample_route_with_hash`] for callers that don't
/// need the `route_config_hash`.
pub fn create_sample_route(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    salt: [u8; 32],
) -> Pubkey {
    create_sample_route_with_hash(svm, payer, owner, mint, salt).0
}

/// Deploy a second loader-owned-executable program (reusing the adapter binary)
/// at a fresh id — a *distinct* valid adapter for `manage_route` set/replace tests.
pub fn deploy_extra_adapter(svm: &mut LiteSVM) -> Pubkey {
    let id = Pubkey::new_unique();
    let so = fs::read(adapter_program_so_path()).expect("adapter .so readable");
    svm.add_program(id, so.as_slice())
        .expect("deploy extra adapter program");
    id
}

/// Plant a real, non-executable account — a stand-in for a mistyped pubkey
/// pointing at a data account rather than a program — so adapter-validation
/// reject paths are representative (the account exists but is not a program).
pub fn plant_non_executable_account(svm: &mut LiteSVM) -> Pubkey {
    let key = Pubkey::new_unique();
    let account = solana_sdk::account::Account {
        lamports: svm.minimum_balance_for_rent_exemption(8),
        data: vec![0u8; 8],
        owner: system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    svm.set_account(key, account).unwrap();
    key
}

/// Build a `manage_route` instruction. When `params.adapters` is `Some`, its
/// program accounts ride in `remaining_accounts`, one per entry in the same
/// order, so the program can loader-owned-executable-check each.
pub fn manage_route_ix(
    owner: &Pubkey,
    route: &Pubkey,
    params: ManageRouteParams,
    adapter_accounts: &[Pubkey],
) -> Instruction {
    let mut accounts = sra_handler::accounts::ManageRoute {
        owner: *owner,
        route: *route,
    }
    .to_account_metas(None);
    for adapter in adapter_accounts {
        accounts.push(AccountMeta::new_readonly(*adapter, false));
    }
    Instruction {
        program_id: PROGRAM_ID,
        accounts,
        data: sra_handler::instruction::ManageRoute { params }.data(),
    }
}

pub fn derive_fill_receipt_pda(route: &Pubkey, fill_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_FILL_RECEIPT, route.as_ref(), fill_id], &PROGRAM_ID)
}

/// Plant a `FillReceipt` in the given lifecycle state via `set_account` — lets
/// tests reach states no instruction produces yet. Mirrors `seed_mint_nonce_account`.
pub fn seed_fill_receipt_with_status(
    svm: &mut LiteSVM,
    route: &Pubkey,
    fill_id: [u8; 32],
    amount: u64,
    execution_status: ExecutionStatus,
) -> Pubkey {
    let (pda, bump) = derive_fill_receipt_pda(route, &fill_id);
    let receipt = FillReceipt {
        route: *route,
        fill_id,
        amount,
        expected_amount: amount,
        execution_status,
        finalized_at: 0,
        bump,
    };
    let mut data = <FillReceipt as Discriminator>::DISCRIMINATOR.to_vec();
    receipt.serialize(&mut data).unwrap();

    let account = solana_sdk::account::Account {
        lamports: svm.minimum_balance_for_rent_exemption(data.len()),
        data,
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    svm.set_account(pda, account).unwrap();
    pda
}

// --- Destination fill verification (verify_across_fill) ---

pub fn derive_verified_fill_pda(relay_hash: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_VERIFIED_FILL, ADAPTER_PROGRAM_ID.as_ref(), relay_hash],
        &ADAPTER_PROGRAM_ID,
    )
}

/// Compute the destination relay hash the adapter re-derives on-chain (the fill
/// lands on this chain, so the bound chain id is `SOLANA_CHAIN_ID`).
pub fn relay_hash_for(relay_data: &RelayData) -> [u8; 32] {
    sra_across_adapter::utils::relay_hash::compute_relay_hash(relay_data, SOLANA_CHAIN_ID)
        .expect("relay hash should serialize")
}

/// A `RelayData` whose route-bound fields (`recipient`, `output_token`,
/// `origin_chain_id`) are caller-chosen so route-binding tests can match or
/// mismatch them; every other field is a fixed non-zero sample.
pub fn sample_relay_data(
    recipient: Pubkey,
    output_token: Pubkey,
    origin_chain_id: u64,
    output_amount: u64,
) -> RelayData {
    RelayData {
        depositor: Pubkey::new_from_array([0x11; 32]),
        recipient,
        exclusive_relayer: Pubkey::new_from_array([0x22; 32]),
        input_token: Pubkey::new_from_array([0x33; 32]),
        output_token,
        input_amount: [0x44; 32],
        output_amount,
        origin_chain_id,
        deposit_id: [0x55; 32],
        fill_deadline: 1_900_000_000,
        exclusivity_deadline: 1_899_900_000,
        message: vec![0xde, 0xad, 0xbe, 0xef],
    }
}

/// Build a `verify_across_fill` instruction. `fill_status` and `verified_fill`
/// are both derived from `relay_hash`, exactly as the on-chain seeds require.
pub fn verify_across_fill_ix(
    payer: &Pubkey,
    route: &Pubkey,
    relay_hash: [u8; 32],
    relay_data: RelayData,
) -> Instruction {
    let (fill_status, _) = derive_fill_status_pda(&relay_hash);
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    Instruction {
        program_id: ADAPTER_PROGRAM_ID,
        accounts: sra_across_adapter::accounts::VerifyAcrossFill {
            payer: *payer,
            fill_status,
            route: *route,
            verified_fill,
            across_program: ACROSS_SPOKE_POOL_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_across_adapter::instruction::VerifyAcrossFill {
            relay_hash,
            relay_data,
        }
        .data(),
    }
}

pub fn fetch_verified_fill(svm: &LiteSVM, pda: &Pubkey) -> sra_across_adapter::state::VerifiedFill {
    let acct = svm.get_account(pda).expect("verified_fill account exists");
    sra_across_adapter::state::VerifiedFill::try_deserialize(&mut acct.data.as_slice())
        .expect("valid VerifiedFill")
}

// --- Destination fill finalization (finalize_fill) ---

/// Build a `finalize_fill` instruction. All vault/authority accounts are derived
/// from the route + mint exactly as the on-chain constraints require.
pub fn finalize_fill_ix(
    payer: &Pubkey,
    route: &Pubkey,
    route_config_hash: [u8; 32],
    verified_fill: &Pubkey,
    fill_id: [u8; 32],
    mint: &Pubkey,
) -> Instruction {
    let (execution_authority, _) = derive_execution_authority_pda(route);
    let route_vault = derive_ata(route, mint);
    let execution_vault = derive_ata(&execution_authority, mint);
    let (fill_receipt, _) = derive_fill_receipt_pda(route, &fill_id);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::FinalizeFill {
            payer: *payer,
            route: *route,
            verified_fill: *verified_fill,
            fill_receipt,
            expected_mint: *mint,
            route_vault,
            execution_authority,
            execution_vault,
            token_program: TOKEN_PROGRAM_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::FinalizeFill {
            fill_id,
            route_config_hash,
        }
        .data(),
    }
}

pub fn fetch_fill_receipt(svm: &LiteSVM, pda: &Pubkey) -> FillReceipt {
    let acct = svm.get_account(pda).expect("fill_receipt account exists");
    FillReceipt::try_deserialize(&mut acct.data.as_slice()).expect("valid FillReceipt")
}

/// Build a `recover_funds` instruction. `owner_token_account` is explicit (not
/// derived) so payout-pinning tests can supply a non-canonical target.
pub fn recover_funds_ix(
    owner: &Pubkey,
    route: &Pubkey,
    fill_id: &[u8; 32],
    mint: &Pubkey,
    owner_token_account: &Pubkey,
) -> Instruction {
    let (execution_authority, _) = derive_execution_authority_pda(route);
    let execution_vault = derive_ata(&execution_authority, mint);
    let (fill_receipt, _) = derive_fill_receipt_pda(route, fill_id);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::RecoverFunds {
            owner: *owner,
            route: *route,
            fill_receipt,
            expected_mint: *mint,
            execution_authority,
            execution_vault,
            owner_token_account: *owner_token_account,
            token_program: TOKEN_PROGRAM_ID,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::RecoverFunds {}.data(),
    }
}

/// A valid `VerifiedFillData` bound to the given route/mint/fill, with
/// `amount == expected_amount` (Across exact fill) and `source_chain_id` matching
/// the sample route. Mutate fields to build forgery cases.
pub fn sample_verified_fill_data(
    route: Pubkey,
    mint: Pubkey,
    fill_id: [u8; 32],
    amount: u64,
) -> sra_common::types::VerifiedFillData {
    sra_common::types::VerifiedFillData {
        adapter: ADAPTER_PROGRAM_ID,
        route,
        fill_id,
        mint,
        amount,
        expected_amount: amount,
        source_chain_id: 42_161,
        created_at: 0,
        bump: 0,
    }
}

/// Serialize a `VerifiedFillData` exactly as the adapter writes it on-chain:
/// 8-byte discriminator + Borsh body.
pub fn verified_fill_account_bytes(vf: &sra_common::types::VerifiedFillData) -> Vec<u8> {
    let mut data = sra_common::types::VERIFIED_FILL_DISCRIMINATOR.to_vec();
    vf.serialize(&mut data).unwrap();
    data
}

/// Plant a `verified_fill`-shaped account at the verified-fill PDA for `fill_id`,
/// owned by `owner`, holding `data`. The handler reads `verified_fill` as a raw
/// `UncheckedAccount` (no seed/owner constraint), so the address is cosmetic —
/// `fill_id` picks a stable, realistic one. Used to inject forged/edge evidence
/// the genuine `verify_across_fill` flow can never produce.
pub fn fabricate_verified_fill(
    svm: &mut LiteSVM,
    fill_id: [u8; 32],
    owner: Pubkey,
    data: Vec<u8>,
) -> Pubkey {
    let (pda, _) = derive_verified_fill_pda(&fill_id);
    svm.set_account(
        pda,
        solana_sdk::account::Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len().max(1)),
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
    pda
}
