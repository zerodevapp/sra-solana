use super::*;
use anchor_lang::AccountSerialize;
use litesvm_token::spl_token::state::{Account, Mint};
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    transaction::TransactionError,
};

impl PassiveFixture {
    pub fn for_mint(
        svm: &mut LiteSVM,
        fixture: &Self,
        source_mint: Pubkey,
        adapter_program: Pubkey,
        across_state: Pubkey,
        across_vault: Pubkey,
    ) -> Self {
        let target_decimals = fetch_address_book_entry(
            svm,
            &fixture.address_book_entry(fixture.target_chain_id, &fixture.target_token),
        )
        .target_decimals;
        setup_address_book_entry(
            svm,
            &fixture.payer,
            &fixture.owner,
            &fixture.address_book_root,
            &source_mint,
            fixture.target_chain_id,
            fixture.target_token,
            target_decimals,
        );
        let accounts = TriggerAccounts::derive(&fixture.sra_config, &source_mint, &adapter_program);
        create_ata(svm, &fixture.payer, &fixture.sra_config, &source_mint);
        mint_to(
            svm,
            &fixture.payer,
            &fixture.payer,
            &source_mint,
            &accounts.sra_vault,
            SRA_STARTING_BALANCE,
        );
        let owner_ata = create_ata(svm, &fixture.payer, &fixture.owner.pubkey(), &source_mint);
        Self {
            payer: fixture.payer.insecure_clone(),
            owner: fixture.owner.insecure_clone(),
            initiator: fixture.initiator.insecure_clone(),
            fee_admin: fixture.fee_admin.insecure_clone(),
            address_book_root: fixture.address_book_root,
            handler_data: fixture.handler_data.clone(),
            sra_config: fixture.sra_config,
            source_mint,
            target_chain_id: fixture.target_chain_id,
            target_token: fixture.target_token,
            adapter_program,
            accounts,
            owner_ata,
            across_state,
            across_vault,
        }
    }

    pub fn from_existing_accounts(
        payer: &Keypair,
        owner: &Keypair,
        initiator: &Keypair,
        handler_data: SourceHandlerData,
        sra_config: Pubkey,
        source_mint: Pubkey,
        target_chain_id: u64,
        adapter_program: Pubkey,
        across_state: Pubkey,
        across_vault: Pubkey,
    ) -> Self {
        let address_book_root = handler_data.address_book_root;
        let target_token = handler_data.target_token;
        Self {
            payer: payer.insecure_clone(),
            owner: owner.insecure_clone(),
            initiator: initiator.insecure_clone(),
            fee_admin: Keypair::new(),
            address_book_root,
            handler_data,
            sra_config,
            source_mint,
            target_chain_id,
            target_token,
            adapter_program,
            accounts: TriggerAccounts::derive(&sra_config, &source_mint, &adapter_program),
            owner_ata: derive_ata(&owner.pubkey(), &source_mint),
            across_state,
            across_vault,
        }
    }
}

pub fn set_source_balance(svm: &mut LiteSVM, fx: &PassiveFixture, amount: u64) {
    let mut vault_account = svm.get_account(&fx.accounts.sra_vault).unwrap();
    let mut vault = Account::unpack(&vault_account.data).unwrap();
    vault.amount = amount;
    vault.pack_into_slice(&mut vault_account.data);
    svm.set_account(fx.accounts.sra_vault, vault_account)
        .unwrap();
    let mut mint_account = svm.get_account(&fx.source_mint).unwrap();
    let mut mint = Mint::unpack(&mint_account.data).unwrap();
    mint.supply = amount;
    mint.pack_into_slice(&mut mint_account.data);
    svm.set_account(fx.source_mint, mint_account).unwrap();
}

#[derive(Debug, PartialEq, Eq)]
pub struct PassiveSnapshot {
    sra_vault_balance: u64,
    fee_recipient_balance: u64,
    adapter_custody_balance: u64,
    source_mint_supply: u64,
    sra_config: Vec<u8>,
    across_vault_balance: u64,
    circle_event_data: Option<Vec<u8>>,
}

pub fn token_balance_or_zero(svm: &LiteSVM, address: &Pubkey) -> u64 {
    svm.get_account(address)
        .map(|_| fetch_token_balance(svm, address))
        .unwrap_or(0)
}

pub fn passive_snapshot(
    svm: &LiteSVM,
    fx: &PassiveFixture,
    circle_event: Option<Pubkey>,
) -> PassiveSnapshot {
    let fee_recipient = fetch_fee_config(svm, &derive_fee_config_pda().0).fee_recipient;
    PassiveSnapshot {
        sra_vault_balance: fetch_token_balance(svm, &fx.accounts.sra_vault),
        fee_recipient_balance: token_balance_or_zero(
            svm,
            &derive_ata(&fee_recipient, &fx.source_mint),
        ),
        adapter_custody_balance: token_balance_or_zero(svm, &fx.accounts.adapter_custody),
        source_mint_supply: fetch_mint_supply(svm, &fx.source_mint),
        sra_config: svm.get_account(&fx.sra_config).unwrap().data,
        across_vault_balance: fetch_token_balance(svm, &fx.across_vault),
        circle_event_data: circle_event
            .and_then(|address| svm.get_account(&address))
            .map(|account| account.data),
    }
}

pub fn set_sra_config(svm: &mut LiteSVM, sra: &Pubkey, mutate: impl FnOnce(&mut SraConfig)) {
    let mut state = fetch_sra_config(svm, sra);
    mutate(&mut state);
    let mut account = svm.get_account(sra).unwrap();
    state
        .try_serialize(&mut account.data.as_mut_slice())
        .unwrap();
    svm.set_account(*sra, account).unwrap();
}

pub fn cctp_pair(
    fx: &PassiveFixture,
    params: PrepareParams,
    cctp: CctpDepositParams,
    fee_ata: Option<Pubkey>,
) -> (Instruction, Instruction, Keypair) {
    let (execute, event) = execute_deposit_cctp_ix(&fx.payer.pubkey(), fx, &cctp);
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        fee_ata,
        params,
        &execute,
    );
    (trigger, execute, event)
}

pub fn across_pair(
    fx: &PassiveFixture,
    params: PrepareParams,
    across: AcrossDepositParams,
    fee_ata: Option<Pubkey>,
) -> (Instruction, Instruction) {
    let execute = execute_deposit_across_ix(&fx.payer.pubkey(), fx, &params, across);
    let address_book_entry = fx.address_book_entry(fx.target_chain_id, &fx.target_token);
    let trigger = trigger_ix(
        &fx.payer.pubkey(),
        &fx.initiator.pubkey(),
        &fx.accounts,
        &address_book_entry,
        fee_ata,
        params,
        &execute,
    );
    (trigger, execute)
}

pub fn assert_rejected_unchanged(
    svm: &mut LiteSVM,
    fx: &PassiveFixture,
    instructions: &[Instruction],
    signers: &[&Keypair],
    circle_event: Option<Pubkey>,
    context: &str,
) -> TransactionError {
    let before = passive_snapshot(svm, fx, circle_event);
    let error = send_instructions(svm, instructions, &fx.payer, signers)
        .expect_err(context)
        .err
        .clone();
    assert_eq!(passive_snapshot(svm, fx, circle_event), before, "{context}");
    error
}

pub fn assert_sra_error(error: TransactionError, instruction: u8, expected: SraError) {
    assert_eq!(
        error,
        TransactionError::InstructionError(instruction, InstructionError::Custom(expected.into()),)
    );
}
