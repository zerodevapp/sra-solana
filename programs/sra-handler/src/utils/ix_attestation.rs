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

pub fn verify_next_adapter_ix(
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
