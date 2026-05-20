use anchor_lang::{error, prelude::*, solana_program::sysvar::instructions};
use sra_common::{
    errors::SraError,
    instruction_data::{
        decode_anchor_instruction_data, encode_anchor_instruction_data, instruction_data_hash,
    },
    source_deposit::{AcrossDepositParams, PrepareParams, ValidateAndPrepareArgs},
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

    let args = decode_anchor_instruction_data::<ValidateAndPrepareArgs>(
        "validate_and_prepare",
        &prev_ix.data,
    )?;
    require!(
        instruction_data_hash(&encode_anchor_instruction_data(
            "execute_deposit_across",
            deposit
        )) == args.paired_adapter_ix_data_hash,
        SraError::PairedInstructionMismatch
    );
    require!(
        AcrossDepositParams::from_prepare(
            &args.params,
            deposit.exclusive_relayer,
            deposit.fill_deadline_offset,
            deposit.exclusivity_period,
        ) == *deposit,
        SraError::PairedInstructionMismatch
    );
    Ok(args.params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{
        error,
        solana_program::instruction::{AccountMeta, Instruction},
    };
    use sra_common::{
        instruction_data::{encode_anchor_instruction_data, instruction_data_hash},
        test_utils::{
            empty_instruction, sample_across_deposit, sample_prepare_params,
            InstructionsSysvarFixture,
        },
        types::SourceHandlerData,
    };

    fn sample_handler_data() -> SourceHandlerData {
        SourceHandlerData {
            address_book_root: Pubkey::new_unique(),
            target_chain_id: 42_161,
            target_handler: [0x11; 32],
            target_adapter: [0x22; 32],
            recipient: [0x33; 32],
            app_id: [0x44; 32],
            target_token: [0xAB; 32],
            allowed_slippage_bps: 50,
            allowed_adapters: vec![Pubkey::new_unique()],
        }
    }

    fn previous_validate_ix(
        sra_config: Pubkey,
        source_mint: Pubkey,
        adapter_program: Pubkey,
        prepare: &PrepareParams,
        paired_adapter_ix_data_hash: [u8; 32],
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
            data: encode_anchor_instruction_data(
                "validate_and_prepare",
                &ValidateAndPrepareArgs {
                    handler_data: sample_handler_data(),
                    params: prepare.clone(),
                    paired_adapter_ix_data_hash,
                },
            ),
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
        let mut bumped = sample_prepare_params().output_amount;
        // Increment the big-endian u256 by one in the last byte (sample fits well under u8::MAX).
        bumped[31] = bumped[31].wrapping_add(1);
        deposit.output_amount = bumped;
    }

    fn mutate_malformed_data(ix: &mut Instruction, _deposit: &mut AcrossDepositParams) {
        ix.data.truncate(7);
    }

    #[test]
    fn verify_previous_handler_ix_accepts_matching_instruction() {
        let prepare = sample_prepare_params();
        let deposit = sample_across_deposit(&prepare);
        let expected_hash = instruction_data_hash(&encode_anchor_instruction_data(
            "execute_deposit_across",
            &deposit,
        ));
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let previous = previous_validate_ix(
            sra_config,
            source_mint,
            adapter_program,
            &prepare,
            expected_hash,
        );
        let current = empty_instruction();
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
    }

    #[test]
    fn verify_previous_handler_ix_rejects_missing_previous_instruction() {
        let prepare = sample_prepare_params();
        let deposit = sample_across_deposit(&prepare);
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let mut sysvar = InstructionsSysvarFixture::new(0, &[empty_instruction()]);
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
        let prepare = sample_prepare_params();
        let expected_hash = instruction_data_hash(&encode_anchor_instruction_data(
            "execute_deposit_across",
            &sample_across_deposit(&prepare),
        ));
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
            let mut previous = previous_validate_ix(
                sra_config,
                source_mint,
                adapter_program,
                &prepare,
                expected_hash,
            );
            let current = empty_instruction();
            let mut deposit = sample_across_deposit(&prepare);
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

    #[test]
    fn verify_previous_handler_ix_rejects_data_hash_mismatch() {
        let prepare = sample_prepare_params();
        let mut deposit = sample_across_deposit(&prepare);
        let expected_hash = instruction_data_hash(&encode_anchor_instruction_data(
            "execute_deposit_across",
            &deposit,
        ));
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let previous = previous_validate_ix(
            sra_config,
            source_mint,
            adapter_program,
            &prepare,
            expected_hash,
        );

        deposit.fill_deadline_offset += 1;
        let current = empty_instruction();
        let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, current]);
        let account_info = sysvar.account_info();

        let err = verify_previous_handler_ix(
            &account_info,
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("current adapter data hash must match the previous handler attestation");

        assert_eq!(err, error!(SraError::PairedInstructionMismatch));
    }
}
