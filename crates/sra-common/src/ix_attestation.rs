//! Source-leg paired-instruction attestation, shared by every bridge adapter.

use anchor_lang::{error, prelude::*, solana_program::sysvar::instructions};

use crate::errors::SraError;
use crate::instruction_data::{
    decode_anchor_instruction_data, encode_anchor_instruction_data, instruction_data_hash,
};
use crate::source_deposit::{AcrossDepositParams, CctpDepositParams, PrepareParams, TriggerArgs};

// Account-slot indices in the handler's `Trigger` ix that the
// adapter verifier needs to pin. Drift would silently break attestation, so
// these mirror `programs/sra-handler/src/instructions/source/trigger.rs`
// account order.
const HANDLER_ACCT_SRA_CONFIG: usize = 2;
const HANDLER_ACCT_SOURCE_MINT: usize = 3;
const HANDLER_ACCT_ADAPTER_PROGRAM: usize = 8;
const MIN_HANDLER_ACCOUNTS: usize = HANDLER_ACCT_ADAPTER_PROGRAM + 1;

/// An adapter's deposit params, pairable with the handler's
/// `trigger` through instruction-data attestation.
pub trait PairedDeposit: AnchorSerialize + PartialEq + Sized {
    /// The adapter's execute-instruction name (its Anchor discriminator).
    const ADAPTER_IX_NAME: &'static str;

    /// Recompute the expected deposit from the handler-attested prepare
    /// params plus `self`'s bridge-specific execution fields.
    fn recompute(&self, prepare: &PrepareParams) -> Result<Self>;
}

impl PairedDeposit for AcrossDepositParams {
    const ADAPTER_IX_NAME: &'static str = "execute_deposit_across";

    fn recompute(&self, prepare: &PrepareParams) -> Result<Self> {
        Self::from_prepare(
            prepare,
            self.exclusive_relayer,
            self.fill_deadline_offset,
            self.exclusivity_period,
        )
    }
}

impl PairedDeposit for CctpDepositParams {
    const ADAPTER_IX_NAME: &'static str = "execute_deposit_cctp";

    fn recompute(&self, prepare: &PrepareParams) -> Result<Self> {
        Self::from_prepare(prepare, self.max_fee, self.min_finality_threshold)
    }
}

/// Verifies that the previous instruction is the handler's
/// `trigger` paired to this adapter leg: pins the handler's
/// account slots, requires the handler-attested ix-data hash to equal this
/// leg's encoded data, recomputes the deposit from the attested prepare
/// params, and requires equality. Returns the attested `PrepareParams`.
pub fn verify_previous_trigger_ix<D: PairedDeposit>(
    instructions_sysvar: &AccountInfo,
    sra_config: &Pubkey,
    source_mint: &Pubkey,
    adapter_program: &Pubkey,
    deposit: &D,
) -> Result<PrepareParams> {
    let prev_ix = instructions::get_instruction_relative(-1, instructions_sysvar)
        .map_err(|_| error!(SraError::MissingPairedInstruction))?;

    require!(
        prev_ix.program_id == crate::ID,
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

    let args = decode_anchor_instruction_data::<TriggerArgs>("trigger", &prev_ix.data)?;
    require!(
        instruction_data_hash(&encode_anchor_instruction_data(D::ADAPTER_IX_NAME, deposit))
            == args.paired_adapter_ix_data_hash,
        SraError::PairedInstructionMismatch
    );
    let expected_deposit = deposit
        .recompute(&args.params)
        .map_err(|_| error!(SraError::PairedInstructionMismatch))?;
    require!(
        expected_deposit == *deposit,
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
    use std::fmt::Debug;

    use crate::test_utils::{
        empty_instruction, sample_across_deposit, sample_cctp_deposit, sample_prepare_params,
        InstructionsSysvarFixture,
    };
    fn previous_trigger_ix(
        sra_config: Pubkey,
        source_mint: Pubkey,
        adapter_program: Pubkey,
        prepare: &PrepareParams,
        paired_adapter_ix_data_hash: [u8; 32],
    ) -> Instruction {
        Instruction {
            program_id: crate::ID,
            accounts: vec![
                AccountMeta::new(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(sra_config, false),
                AccountMeta::new_readonly(source_mint, false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(Pubkey::new_unique(), false),
                AccountMeta::new(Pubkey::new_unique(), false),
                AccountMeta::new_readonly(adapter_program, false),
            ],
            data: encode_anchor_instruction_data(
                "trigger",
                &TriggerArgs {
                    params: prepare.clone(),
                    paired_adapter_ix_data_hash,
                },
            ),
        }
    }

    fn expected_hash<D: PairedDeposit>(deposit: &D) -> [u8; 32] {
        instruction_data_hash(&encode_anchor_instruction_data(D::ADAPTER_IX_NAME, deposit))
    }

    /// The full accept/reject battery, run once per adapter deposit type.
    /// `mutate_shared_field` flips a field of the shared 5-field prefix;
    /// `mutate_bridge_field` flips a bridge-specific execution field — both
    /// must surface as a pairing mismatch (hash or recompute inequality).
    fn run_battery<D: PairedDeposit + Clone + Debug>(
        sample: fn(&PrepareParams) -> D,
        mutate_shared_field: fn(&mut D),
        mutate_bridge_field: fn(&mut D),
    ) {
        let prepare = sample_prepare_params();
        let sra_config = Pubkey::new_unique();
        let source_mint = Pubkey::new_unique();
        let adapter_program = Pubkey::new_unique();
        let matching_previous = |prepare: &PrepareParams, deposit: &D| {
            previous_trigger_ix(
                sra_config,
                source_mint,
                adapter_program,
                prepare,
                expected_hash(deposit),
            )
        };

        // Matching previous ix must pass and return the attested prepare.
        let deposit = sample(&prepare);
        let mut sysvar = InstructionsSysvarFixture::new(
            1,
            &[matching_previous(&prepare, &deposit), empty_instruction()],
        );
        let recovered = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect("matching previous ix must pass");
        assert_eq!(recovered, prepare);

        // Missing adjacent previous ix must reject.
        let mut sysvar = InstructionsSysvarFixture::new(0, &[empty_instruction()]);
        let err = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("missing adjacent previous ix must reject");
        assert_eq!(err, error!(SraError::MissingPairedInstruction));

        // Mutated previous instructions must reject with the named error.
        type IxMutator = fn(&mut Instruction);
        let cases: [(&str, IxMutator, anchor_lang::error::Error); 7] = [
            (
                "wrong program",
                |ix| ix.program_id = Pubkey::new_unique(),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "short account list",
                |ix| ix.accounts.truncate(MIN_HANDLER_ACCOUNTS - 1),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "wrong sra_config",
                |ix| ix.accounts[HANDLER_ACCT_SRA_CONFIG].pubkey = Pubkey::new_unique(),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong source_mint",
                |ix| ix.accounts[HANDLER_ACCT_SOURCE_MINT].pubkey = Pubkey::new_unique(),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "wrong adapter_program",
                |ix| ix.accounts[HANDLER_ACCT_ADAPTER_PROGRAM].pubkey = Pubkey::new_unique(),
                error!(SraError::PairedInstructionMismatch),
            ),
            (
                "malformed data",
                |ix| ix.data.truncate(7),
                error!(SraError::InvalidPairedInstruction),
            ),
            (
                "wrong discriminator",
                |ix| ix.data[..8].fill(0),
                error!(SraError::InvalidPairedInstruction),
            ),
        ];
        for (name, mutate, expected) in cases {
            let deposit = sample(&prepare);
            let mut previous = matching_previous(&prepare, &deposit);
            mutate(&mut previous);
            let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, empty_instruction()]);
            let err = verify_previous_trigger_ix(
                &sysvar.account_info(),
                &sra_config,
                &source_mint,
                &adapter_program,
                &deposit,
            )
            .unwrap_err();
            assert_eq!(err, expected, "{name} must reject");
        }

        // A shared-prefix deposit field mutated after pairing must reject.
        let mut deposit = sample(&prepare);
        let previous = matching_previous(&prepare, &deposit);
        mutate_shared_field(&mut deposit);
        let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, empty_instruction()]);
        let err = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("mutated shared deposit field must reject");
        assert_eq!(err, error!(SraError::PairedInstructionMismatch));

        let deposit = sample(&prepare);
        let mut mismatched_prepare = prepare.clone();
        mismatched_prepare.output_amount[31] = mismatched_prepare.output_amount[31].wrapping_add(1);
        let previous = matching_previous(&mismatched_prepare, &deposit);
        let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, empty_instruction()]);
        let err = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("recomputed deposit must equal the supplied deposit");
        assert_eq!(err, error!(SraError::PairedInstructionMismatch));

        // A handler payload whose attested fee exceeds the input can never
        // have executed, but the verifier must keep its error surface closed:
        // conversion failure is a pairing mismatch, not a leaked overflow.
        let mut bad_prepare = prepare.clone();
        bad_prepare.protocol_fee_amount = bad_prepare.input_amount + 1;
        let deposit = sample(&prepare);
        let mut sysvar = InstructionsSysvarFixture::new(
            1,
            &[
                matching_previous(&bad_prepare, &deposit),
                empty_instruction(),
            ],
        );
        let err = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("fee above input must reject");
        assert_eq!(err, error!(SraError::PairedInstructionMismatch));

        // A bridge-specific field mutated after pairing must break the hash.
        let mut deposit = sample(&prepare);
        let previous = matching_previous(&prepare, &deposit);
        mutate_bridge_field(&mut deposit);
        let mut sysvar = InstructionsSysvarFixture::new(1, &[previous, empty_instruction()]);
        let err = verify_previous_trigger_ix(
            &sysvar.account_info(),
            &sra_config,
            &source_mint,
            &adapter_program,
            &deposit,
        )
        .expect_err("current adapter data hash must match the previous handler attestation");
        assert_eq!(err, error!(SraError::PairedInstructionMismatch));
    }

    #[test]
    fn across_attestation_battery() {
        run_battery(
            sample_across_deposit,
            |d: &mut AcrossDepositParams| d.output_amount[31] = d.output_amount[31].wrapping_add(1),
            |d: &mut AcrossDepositParams| d.fill_deadline_offset += 1,
        );
    }

    #[test]
    fn cctp_attestation_battery() {
        run_battery(
            sample_cctp_deposit,
            |d: &mut CctpDepositParams| d.output_amount[31] = d.output_amount[31].wrapping_add(1),
            |d: &mut CctpDepositParams| d.max_fee += 1,
        );
    }
}
