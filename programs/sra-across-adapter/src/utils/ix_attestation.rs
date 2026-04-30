use anchor_lang::{error, prelude::*, solana_program::sysvar::instructions};
use sra_common::{
    errors::SraError,
    instruction_data::decode_anchor_instruction_data,
    source_deposit::{AcrossDepositParams, NormalizedDepositIntent, PrepareParams},
};

// Account-slot indices in the handler's `ValidateAndPrepare` ix that the
// adapter verifier needs to pin. Drift would silently break attestation, so
// these mirror `programs/sra-handler/src/instructions/source/validate_and_prepare.rs`
// account order.
const HANDLER_ACCT_SRA_CONFIG: usize = 2;
const HANDLER_ACCT_SOURCE_MINT: usize = 4;
const HANDLER_ACCT_ADAPTER_PROGRAM: usize = 9;
const MIN_HANDLER_ACCOUNTS: usize = HANDLER_ACCT_ADAPTER_PROGRAM + 1;

pub(crate) fn verify_previous_handler_ix(
    instructions_sysvar: &AccountInfo,
    sra_config: &Pubkey,
    source_mint: &Pubkey,
    adapter_program: &Pubkey,
    deposit: &AcrossDepositParams,
) -> Result<PrepareParams> {
    let prev_ix = instructions::get_instruction_relative(-1, instructions_sysvar)
        .map_err(|_| error!(SraError::MissingPairedInstruction))?;

    require!(
        prev_ix.program_id == sra_common::ID,
        SraError::InvalidPairedInstruction
    );
    require!(
        prev_ix.accounts.len() >= MIN_HANDLER_ACCOUNTS,
        SraError::InvalidPairedInstruction
    );
    require!(
        prev_ix.accounts[HANDLER_ACCT_SRA_CONFIG].pubkey == *sra_config,
        SraError::PairedInstructionMismatch
    );
    require!(
        prev_ix.accounts[HANDLER_ACCT_SOURCE_MINT].pubkey == *source_mint,
        SraError::PairedInstructionMismatch
    );
    require!(
        prev_ix.accounts[HANDLER_ACCT_ADAPTER_PROGRAM].pubkey == *adapter_program,
        SraError::PairedInstructionMismatch
    );

    let prepare =
        decode_anchor_instruction_data::<PrepareParams>("validate_and_prepare", &prev_ix.data)?;
    require!(
        NormalizedDepositIntent::from_prepare(&prepare)
            == NormalizedDepositIntent::from_across(deposit),
        SraError::PairedInstructionMismatch
    );
    Ok(prepare)
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

    fn sample_deposit(prepare: &PrepareParams) -> AcrossDepositParams {
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

    fn encode_validate_data(params: &PrepareParams) -> Vec<u8> {
        let mut data =
            sra_common::instruction_data::instruction_discriminator("validate_and_prepare")
                .to_vec();
        params.serialize(&mut data).unwrap();
        data
    }

    fn previous_validate_ix(
        sra_config: Pubkey,
        source_mint: Pubkey,
        adapter_program: Pubkey,
        prepare: &PrepareParams,
    ) -> Instruction {
        Instruction {
            program_id: sra_common::ID,
            accounts: vec![
                AccountMeta::new(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(sra_config, false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(source_mint, false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(adapter_program, false),
            ],
            data: encode_validate_data(prepare),
        }
    }

    fn current_ix() -> Instruction {
        Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![],
        }
    }

    fn mutate_wrong_program(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.program_id = Pubkey::new_unique();
    }

    fn mutate_short_accounts(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.accounts.truncate(MIN_HANDLER_ACCOUNTS - 1);
    }

    fn mutate_wrong_sra_config(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.accounts[HANDLER_ACCT_SRA_CONFIG].pubkey = Pubkey::new_unique();
    }

    fn mutate_wrong_source_mint(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.accounts[HANDLER_ACCT_SOURCE_MINT].pubkey = Pubkey::new_unique();
    }

    fn mutate_wrong_adapter_program(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.accounts[HANDLER_ACCT_ADAPTER_PROGRAM].pubkey = Pubkey::new_unique();
    }

    fn mutate_wrong_output_amount(_ix: &mut Instruction, deposit: &mut AcrossDepositParams) {
        deposit.output_amount = sra_common::source_deposit::handler_output_amount_u256(
            sample_prepare().output_amount + 1,
        );
    }

    fn mutate_malformed_data(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.data.truncate(7);
    }

    #[test]
    fn verify_previous_handler_ix_accepts_matching_instruction() {
        let prepare = sample_prepare();
        let mut deposit = sample_deposit(&prepare);
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let previous = previous_validate_ix(sra_config, source_mint, adapter_program, &prepare);
        let current = current_ix();
        let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, current]);
        let account_info = sysvar.account_info();

        let recovered = verify_previous_handler_ix(
            &account_info,
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect("matching previous ix must pass");

        assert_eq!(recovered, prepare);
        deposit.output_amount =
            sra_common::source_deposit::handler_output_amount_u256(prepare.output_amount);
    }

    #[test]
    fn verify_previous_handler_ix_rejects_missing_previous_instruction() {
        let prepare = sample_prepare();
        let deposit = sample_deposit(&prepare);
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current_ix()]);
        let account_info = sysvar.account_info();

        let err = verify_previous_handler_ix(
            &account_info,
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("missing adjacent previous ix must reject");

        assert_eq!(err, error!(SraError::MissingPairedInstruction));
    }

    #[test]
    fn verify_previous_handler_ix_rejects_mismatch_batch() {
        let prepare = sample_prepare();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let cases = [
            (
                "wrong program",
                mutate_wrong_program as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "short account list",
                mutate_short_accounts as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "wrong sra_config",
                mutate_wrong_sra_config as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong source_mint",
                mutate_wrong_source_mint as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong adapter_program",
                mutate_wrong_adapter_program as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong output_amount",
                mutate_wrong_output_amount as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "malformed data",
                mutate_malformed_data as fn(&mut Instruction, &mut AcrossDepositParams),
                error!(SraError::InvalidPairedInstruction),
            ),
        ];

        for (name, mutate, expected) in cases {
            let mut previous =
                previous_validate_ix(sra_config, source_mint, adapter_program, &prepare);
            let current = current_ix();
            let mut deposit = sample_deposit(&prepare);
            mutate(&mut previous, &mut deposit);
            let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, current]);
            let account_info = sysvar.account_info();

            let err = verify_previous_handler_ix(
                &account_info,
                &sra_config,
                &source_mint,
                &adapter_program,
                &deposit,
            )
            .unwrap_err();

            assert_eq!(err, expected, "{name} must reject");
        }
    }
}
