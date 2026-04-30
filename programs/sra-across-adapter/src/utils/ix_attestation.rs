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
