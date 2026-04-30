use anchor_lang::{error, prelude::*, solana_program::sysvar::instructions};
use sra_common::{
    errors::SraError,
    instruction_data::decode_anchor_instruction_data,
    source_deposit::{AcrossDepositParams, NormalizedDepositIntent, PrepareParams},
};

// Account-slot indices in the adapter's `ExecuteDepositAcross` ix that the
// handler verifier needs to pin. Drift would silently break attestation, so
// these mirror `programs/sra-across-adapter/src/instructions/execute_deposit.rs`
// account order.
const ADAPTER_ACCT_SRA_CONFIG: usize = 2;
const ADAPTER_ACCT_INPUT_MINT: usize = 6;
const MIN_ADAPTER_ACCOUNTS: usize = ADAPTER_ACCT_INPUT_MINT + 1;

pub(crate) fn verify_next_adapter_ix(
    instructions_sysvar: &AccountInfo,
    adapter_program: &Pubkey,
    sra_config: &Pubkey,
    source_mint: &Pubkey,
    prepare: &PrepareParams,
) -> Result<()> {
    let next_ix = instructions::get_instruction_relative(1, instructions_sysvar)
        .map_err(|_| error!(SraError::MissingPairedInstruction))?;

    require!(
        next_ix.program_id == *adapter_program,
        SraError::InvalidPairedInstruction
    );
    require!(
        next_ix.accounts.len() >= MIN_ADAPTER_ACCOUNTS,
        SraError::InvalidPairedInstruction
    );
    require!(
        next_ix.accounts[ADAPTER_ACCT_SRA_CONFIG].pubkey == *sra_config,
        SraError::PairedInstructionMismatch
    );
    require!(
        next_ix.accounts[ADAPTER_ACCT_INPUT_MINT].pubkey == *source_mint,
        SraError::PairedInstructionMismatch
    );

    let deposit = decode_anchor_instruction_data::<AcrossDepositParams>(
        "execute_deposit_across",
        &next_ix.data,
    )?;
    require!(
        NormalizedDepositIntent::from_prepare(prepare)
            == NormalizedDepositIntent::from_across(&deposit),
        SraError::PairedInstructionMismatch
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{
        error,
        solana_program::{
            instruction::{AccountMeta, Instruction},
            sysvar::{
                self,
                instructions::{
                    construct_instructions_data, store_current_index_checked, BorrowedAccountMeta,
                    BorrowedInstruction, ID as INSTRUCTIONS_ID,
                },
            },
        },
        AnchorSerialize,
    };

    struct InstructionsSysvarFixture {
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Vec<u8>,
    }

    impl InstructionsSysvarFixture {
        fn new(current_index: u16, ixs: &[Instruction]) -> Self {
            let borrowed = ixs.iter().map(borrow_instruction).collect::<Vec<_>>();
            let mut data = construct_instructions_data(&borrowed);
            store_current_index_checked(&mut data, current_index).unwrap();
            Self {
                key: INSTRUCTIONS_ID,
                owner: sysvar::ID,
                lamports: 0,
                data,
            }
        }

        fn account_info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(
                &self.key,
                false,
                false,
                &mut self.lamports,
                &mut self.data,
                &self.owner,
                false,
                0,
            )
        }
    }

    fn borrow_instruction(ix: &Instruction) -> BorrowedInstruction<'_> {
        BorrowedInstruction {
            program_id: &ix.program_id,
            accounts: ix
                .accounts
                .iter()
                .map(|meta| BorrowedAccountMeta {
                    pubkey: &meta.pubkey,
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: &ix.data,
        }
    }

    fn sample_prepare() -> PrepareParams {
        PrepareParams {
            input_amount: 123,
            output_amount: 456,
            target_token: [0xAB; 32],
            action_idxs: vec![3, 7],
        }
    }

    fn sample_across(prepare: &PrepareParams) -> AcrossDepositParams {
        AcrossDepositParams {
            input_amount: prepare.input_amount,
            output_amount: sra_common::source_deposit::handler_output_amount_u256(
                prepare.output_amount,
            ),
            target_token: prepare.target_token,
            action_idxs: prepare.action_idxs.clone(),
            exclusive_relayer: Pubkey::new_unique(),
            fill_deadline_offset: 60,
            exclusivity_period: 30,
        }
    }

    fn encode_execute_deposit_data(params: &AcrossDepositParams) -> Vec<u8> {
        let mut data =
            sra_common::instruction_data::instruction_discriminator("execute_deposit_across")
                .to_vec();
        params.serialize(&mut data).unwrap();
        data
    }

    fn current_ix() -> Instruction {
        Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![],
        }
    }

    fn matching_next_ix(
        adapter_program: Pubkey,
        sra_config: Pubkey,
        source_mint: Pubkey,
        prepare: &PrepareParams,
    ) -> Instruction {
        Instruction {
            program_id: adapter_program,
            accounts: vec![
                AccountMeta::new(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(INSTRUCTIONS_ID, false),
                AccountMeta::new_readonly(sra_config, false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(source_mint, false),
            ],
            data: encode_execute_deposit_data(&sample_across(prepare)),
        }
    }

    fn mutate_wrong_program(ix: &mut Instruction) {
        ix.program_id = Pubkey::new_unique();
    }

    fn mutate_short_accounts(ix: &mut Instruction) {
        ix.accounts.truncate(MIN_ADAPTER_ACCOUNTS - 1);
    }

    fn mutate_wrong_sra_config(ix: &mut Instruction) {
        ix.accounts[ADAPTER_ACCT_SRA_CONFIG].pubkey = Pubkey::new_unique();
    }

    fn mutate_wrong_source_mint(ix: &mut Instruction) {
        ix.accounts[ADAPTER_ACCT_INPUT_MINT].pubkey = Pubkey::new_unique();
    }

    fn mutate_wrong_output_amount(ix: &mut Instruction) {
        let prepare = sample_prepare();
        let mut deposit = sample_across(&prepare);
        deposit.output_amount =
            sra_common::source_deposit::handler_output_amount_u256(prepare.output_amount + 1);
        ix.data = encode_execute_deposit_data(&deposit);
    }

    fn mutate_malformed_data(ix: &mut Instruction) {
        ix.data.truncate(7);
    }

    #[test]
    fn verify_next_adapter_ix_accepts_matching_instruction() {
        let prepare = sample_prepare();
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let current = current_ix();
        let next = matching_next_ix(adapter_program, sra_config, source_mint, &prepare);
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
        let account_info = sysvar.account_info();

        verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &prepare,
        )
        .expect("matching next ix must pass");
    }

    #[test]
    fn verify_next_adapter_ix_rejects_missing_next_instruction() {
        let prepare = sample_prepare();
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current_ix()]);
        let account_info = sysvar.account_info();

        let err = verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &prepare,
        )
        .expect_err("missing adjacent next ix must reject");

        assert_eq!(err, error!(SraError::MissingPairedInstruction));
    }

    #[test]
    fn verify_next_adapter_ix_rejects_mismatch_batch() {
        let prepare = sample_prepare();
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let cases = [
            (
                "wrong program",
                mutate_wrong_program as fn(&mut Instruction),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "short account list",
                mutate_short_accounts as fn(&mut Instruction),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "wrong sra_config",
                mutate_wrong_sra_config as fn(&mut Instruction),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong source_mint",
                mutate_wrong_source_mint as fn(&mut Instruction),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong output_amount",
                mutate_wrong_output_amount as fn(&mut Instruction),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "malformed data",
                mutate_malformed_data as fn(&mut Instruction),
                error!(SraError::InvalidPairedInstruction),
            ),
        ];

        for (name, mutate, expected) in cases {
            let current = current_ix();
            let mut next = matching_next_ix(adapter_program, sra_config, source_mint, &prepare);
            mutate(&mut next);
            let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
            let account_info = sysvar.account_info();

            let err = verify_next_adapter_ix(
                &account_info,
                &adapter_program,
                &sra_config,
                &source_mint,
                &prepare,
            )
            .unwrap_err();

            assert_eq!(err, expected, "{name} must reject");
        }
    }
}
