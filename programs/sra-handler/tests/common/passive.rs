use super::*;
use anchor_lang::{AccountSerialize, InstructionData, ToAccountMetas};
use litesvm_token::spl_token::state::{Account, Mint};
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    transaction::TransactionError,
};

pub const SRA_STARTING_BALANCE: u64 = 10_000_000;
const DEFAULT_TARGET_CHAIN_ID: u64 = 42_161;

fn create_canonical_cctp_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    let mint = create_mint(svm, payer, &payer.pubkey(), 6);
    let mint_account = svm.get_account(&mint).unwrap();
    svm.set_account(sra_common::constants::USDC_MINT, mint_account)
        .unwrap();
    sra_common::constants::USDC_MINT
}

pub struct PassiveFixture {
    pub payer: Keypair,
    pub owner: Keypair,
    pub initiator: Keypair,
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
    pub accounts: TriggerAccounts,
    pub owner_ata: Pubkey,
    pub across_state: Pubkey,
    pub across_vault: Pubkey,
}

impl PassiveFixture {
    pub fn new(svm: &mut LiteSVM) -> Self {
        Self::build_cctp(
            svm,
            50,
            vec![CCTP_ADAPTER_PROGRAM_ID],
            DEFAULT_TARGET_CHAIN_ID,
        )
    }

    pub fn new_with(svm: &mut LiteSVM, allowed_slippage_bps: u16) -> Self {
        Self::build_cctp(
            svm,
            allowed_slippage_bps,
            vec![CCTP_ADAPTER_PROGRAM_ID],
            DEFAULT_TARGET_CHAIN_ID,
        )
    }

    /// Default CCTP fixture whose SRA commits to `target_chain_id` — e.g. a
    /// chain with no CCTP domain mapping, to exercise `UnsupportedChain`.
    pub fn new_with_target_chain(svm: &mut LiteSVM, target_chain_id: u64) -> Self {
        Self::build_cctp(svm, 50, vec![CCTP_ADAPTER_PROGRAM_ID], target_chain_id)
    }

    pub fn new_cctp_with_adapter_policy(
        svm: &mut LiteSVM,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
    ) -> Self {
        Self::build_cctp(
            svm,
            allowed_slippage_bps,
            allowed_adapters,
            DEFAULT_TARGET_CHAIN_ID,
        )
    }

    pub fn new_across_with_adapter_policy(
        svm: &mut LiteSVM,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
    ) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let source_mint = create_mint(svm, &payer, &payer.pubkey(), 6);
        Self::build(
            svm,
            payer,
            source_mint,
            ADAPTER_PROGRAM_ID,
            allowed_slippage_bps,
            allowed_adapters,
            DEFAULT_TARGET_CHAIN_ID,
        )
    }

    fn build_cctp(
        svm: &mut LiteSVM,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
        target_chain_id: u64,
    ) -> Self {
        let payer = funded_keypair(svm, 100 * SOL);
        let source_mint = create_canonical_cctp_mint(svm, &payer);
        Self::build(
            svm,
            payer,
            source_mint,
            CCTP_ADAPTER_PROGRAM_ID,
            allowed_slippage_bps,
            allowed_adapters,
            target_chain_id,
        )
    }

    fn build(
        svm: &mut LiteSVM,
        payer: Keypair,
        source_mint: Pubkey,
        adapter_program: Pubkey,
        allowed_slippage_bps: u16,
        allowed_adapters: Vec<Pubkey>,
        target_chain_id: u64,
    ) -> Self {
        let owner = funded_keypair(svm, SOL);
        let initiator = funded_keypair(svm, SOL);

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
        let target_token = sample_target_token();
        let handler_data = SourceHandlerData {
            address_book_root,
            target_chain_id,
            recipient: evm_bytes32(0x33),
            target_token,
            allowed_slippage_bps,
            allowed_adapters,
        };
        let salt = [0xABu8; 32];
        let (sra_config, _) = derive_sra_pda(&owner.pubkey(), &handler_data, &salt);
        send_ix(
            svm,
            initialize_sra_ix(
                &payer.pubkey(),
                &owner.pubkey(),
                &address_book_root,
                &sra_config,
                handler_data.clone(),
                salt,
                initiator.pubkey(),
                source_mint,
            ),
            &payer,
            &[&owner],
        )
        .expect("initialize_sra should succeed");

        setup_address_book_entry(
            svm,
            &payer,
            &owner,
            &address_book_root,
            &source_mint,
            target_chain_id,
            target_token,
            6,
        );

        let accounts = TriggerAccounts::derive(&sra_config, &source_mint, &adapter_program);
        let owner_ata = create_ata(svm, &payer, &owner.pubkey(), &source_mint);
        let across_state = funded_keypair(svm, SOL).pubkey();
        let across_vault = create_ata(svm, &payer, &across_state, &source_mint);
        mint_to(
            svm,
            &payer,
            &payer,
            &source_mint,
            &accounts.sra_vault,
            SRA_STARTING_BALANCE,
        );

        Self {
            payer,
            owner,
            initiator,
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
        self.invoke_full(svm, params, vec![], None, None)
    }

    pub fn invoke_with(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(svm, params, between_ixs, None, None)
    }

    pub fn invoke_with_fee_ata(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        fee_recipient_token_account: Option<Pubkey>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        self.invoke_full(svm, params, vec![], fee_recipient_token_account, None)
    }

    /// Builds the normal 2-IX flow, then replaces the account-meta holding
    /// `victim` with `substitute` in the trigger instruction — substitution
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
            params,
            vec![],
            fee_recipient_token_account,
            Some((victim, substitute)),
        )
    }

    fn invoke_full(
        &self,
        svm: &mut LiteSVM,
        params: PrepareParams,
        between_ixs: Vec<Instruction>,
        fee_recipient_token_account: Option<Pubkey>,
        trigger_meta_substitution: Option<(Pubkey, Pubkey)>,
    ) -> Result<litesvm::types::TransactionMetadata, Box<litesvm::types::FailedTransactionMetadata>>
    {
        let (execute_ix, event_data) =
            execute_deposit_cctp_ix(&self.payer.pubkey(), self, &cctp_params_for(&params));
        let address_book_entry = self.address_book_entry(self.target_chain_id, &self.target_token);
        let mut trigger_ix = trigger_ix(
            &self.payer.pubkey(),
            &self.initiator.pubkey(),
            &self.accounts,
            &address_book_entry,
            fee_recipient_token_account,
            params.clone(),
            &execute_ix,
        );
        if let Some((victim, substitute)) = trigger_meta_substitution {
            let slot = trigger_ix
                .accounts
                .iter()
                .position(|meta| meta.pubkey == victim)
                .expect("victim account must be present in the trigger instruction");
            trigger_ix.accounts[slot].pubkey = substitute;
        }

        let mut ixs = vec![trigger_ix];
        ixs.extend(between_ixs);
        ixs.push(execute_ix);
        send_instructions(svm, &ixs, &self.payer, &[&self.initiator, &event_data])
    }
}

pub fn manage_sra_ix(owner: Pubkey, sra: Pubkey, initiator: Pubkey, active: bool) -> Instruction {
    Instruction {
        program_id: PROGRAM_ID,
        accounts: sra_handler::accounts::ManageSra {
            owner,
            sra_config: sra,
        }
        .to_account_metas(None),
        data: sra_handler::instruction::ManageSra { initiator, active }.data(),
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
