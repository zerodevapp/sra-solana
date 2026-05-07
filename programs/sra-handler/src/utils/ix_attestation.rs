use anchor_lang::{error, prelude::*, solana_program::sysvar::instructions};
use sra_common::{errors::SraError, instruction_data::instruction_data_hash};

// Pinned account slots in `ExecuteDepositAcross`.
const ADAPTER_ACCT_SRA_CONFIG: usize = 2;
const ADAPTER_ACCT_INPUT_MINT: usize = 6;
const MIN_ADAPTER_ACCOUNTS: usize = ADAPTER_ACCT_INPUT_MINT + 1;

pub(crate) fn verify_next_adapter_ix(
    instructions_sysvar: &AccountInfo,
    adapter_program: &Pubkey,
    sra_config: &Pubkey,
    source_mint: &Pubkey,
    expected_ix_data_hash: &[u8; 32],
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
    require!(
        instruction_data_hash(&next_ix.data) == *expected_ix_data_hash,
        SraError::PairedInstructionMismatch
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{
        error,
        solana_program::instruction::{AccountMeta, Instruction},
    };
    use sra_common::{
        instruction_data::encode_anchor_instruction_data,
        source_deposit::PrepareParams,
        test_utils::{
            empty_instruction, sample_across_deposit, sample_prepare_params,
            InstructionsSysvarFixture,
        },
    };

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
                AccountMeta::new_readonly(
                    anchor_lang::solana_program::sysvar::instructions::ID,
                    false,
                ),
                AccountMeta::new_readonly(sra_config, false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(source_mint, false),
            ],
            data: encode_anchor_instruction_data(
                "execute_deposit_across",
                &sample_across_deposit(prepare),
            ),
        }
    }

    fn opaque_next_ix(
        adapter_program: Pubkey,
        sra_config: Pubkey,
        source_mint: Pubkey,
        data: Vec<u8>,
    ) -> Instruction {
        Instruction {
            program_id: adapter_program,
            accounts: vec![
                AccountMeta::new(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(
                    anchor_lang::solana_program::sysvar::instructions::ID,
                    false,
                ),
                AccountMeta::new_readonly(sra_config, false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(source_mint, false),
            ],
            data,
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

    fn mutate_wrong_exclusive_relayer(ix: &mut Instruction, prepare: &PrepareParams) {
        let mut deposit = sample_across_deposit(prepare);
        deposit.exclusive_relayer = Pubkey::new_unique();
        ix.data = encode_anchor_instruction_data("execute_deposit_across", &deposit);
    }

    #[test]
    fn verify_next_adapter_ix_accepts_matching_instruction() {
        let prepare = sample_prepare_params();
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let current = empty_instruction();
        let next = matching_next_ix(adapter_program, sra_config, source_mint, &prepare);
        let expected_hash = instruction_data_hash(&next.data);
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
        let account_info = sysvar.account_info();

        verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &expected_hash,
        )
        .expect("matching next ix must pass");
    }

    #[test]
    fn verify_next_adapter_ix_accepts_opaque_adapter_data_when_hash_matches() {
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let current = empty_instruction();
        let next = opaque_next_ix(
            adapter_program,
            sra_config,
            source_mint,
            vec![0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5],
        );
        let expected_hash = instruction_data_hash(&next.data);
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
        let account_info = sysvar.account_info();

        verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &expected_hash,
        )
        .expect("opaque paired data must pass when hash and pinned slots match");
    }

    #[test]
    fn verify_next_adapter_ix_rejects_missing_next_instruction() {
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let mut sysvar = InstructionsSysvarFixture::new(0, &[empty_instruction()]);
        let account_info = sysvar.account_info();

        let err = verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &[0u8; 32],
        )
        .expect_err("missing adjacent next ix must reject");

        assert_eq!(err, error!(SraError::MissingPairedInstruction));
    }

    #[test]
    fn verify_next_adapter_ix_rejects_mismatch_batch() {
        let prepare = sample_prepare_params();
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
        ];

        for (name, mutate, expected) in cases {
            let current = empty_instruction();
            let mut next = matching_next_ix(adapter_program, sra_config, source_mint, &prepare);
            mutate(&mut next);
            let expected_hash = instruction_data_hash(&next.data);
            let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
            let account_info = sysvar.account_info();

            let err = verify_next_adapter_ix(
                &account_info,
                &adapter_program,
                &sra_config,
                &source_mint,
                &expected_hash,
            )
            .unwrap_err();

            assert_eq!(err, expected, "{name} must reject");
        }
    }

    #[test]
    fn verify_next_adapter_ix_rejects_data_hash_mismatch() {
        let prepare = sample_prepare_params();
        let adapter_program = Pubkey::new_unique();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let current = empty_instruction();
        let mut next = matching_next_ix(adapter_program, sra_config, source_mint, &prepare);
        let expected_hash = sra_common::instruction_data::instruction_data_hash(&next.data);
        mutate_wrong_exclusive_relayer(&mut next, &prepare);
        let mut sysvar = InstructionsSysvarFixture::new(0, &[current, next]);
        let account_info = sysvar.account_info();

        let err = verify_next_adapter_ix(
            &account_info,
            &adapter_program,
            &sra_config,
            &source_mint,
            &expected_hash,
        )
        .expect_err("exclusive relayer data-hash mismatch must reject");

        assert_eq!(err, error!(SraError::PairedInstructionMismatch));
    }
}
