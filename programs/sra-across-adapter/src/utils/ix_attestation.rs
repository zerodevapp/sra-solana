//! Adapter-side wrapper for the bidirectional IX attestation defined in
//! `sra_common::ix_attestation`. Step 15's `execute_deposit_across` will call
//! `verify_handler_ix` to confirm the immediately-preceding instruction is
//! `sra_handler::validate_and_prepare` for this SRA with byte-identical
//! routing fields. The pure comparison logic and the unit tests live in
//! sra-common; this wrapper only loads the previous instruction from the
//! Instructions sysvar and forwards it.

use anchor_lang::prelude::*;
use sra_common::{
    errors::SraError,
    handler_ix_layout,
    ix_attestation::{verify_ix_routing_payload, IxLayout, Routing},
};

/// Confirm the immediately-preceding instruction is
/// `sra_handler::validate_and_prepare` for `expected_sra_config`, and that
/// the routing payload it pinned matches `expected`.
///
/// Returns `SraError::InvalidPrecedingInstruction` on any of:
/// - no instruction at relative offset -1 (standalone IX 2);
/// - IX 1 program id does not match `expected_handler_program`;
/// - IX 1 discriminator is not `VALIDATE_AND_PREPARE_DISCRIMINATOR`;
/// - IX 1's `sra_config` slot does not match `expected_sra_config`;
/// - any of the routing fields differ from `expected`.
pub fn verify_handler_ix(
    instructions_sysvar: &AccountInfo,
    expected_handler_program: &Pubkey,
    expected_sra_config: &Pubkey,
    expected: &Routing,
) -> Result<()> {
    #[allow(deprecated)]
    let prev_ix = anchor_lang::solana_program::sysvar::instructions::get_instruction_relative(
        -1,
        instructions_sysvar,
    )
    .map_err(|_| error!(SraError::InvalidPrecedingInstruction))?;

    let layout = IxLayout {
        expected_program: expected_handler_program,
        expected_discriminator: &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
        expected_sra_config,
        sra_config_account_index: handler_ix_layout::ACCT_SRA_CONFIG,
        data_input_amount_offset: handler_ix_layout::DATA_INPUT_AMOUNT_OFFSET,
        data_output_amount_offset: handler_ix_layout::DATA_OUTPUT_AMOUNT_OFFSET,
        data_target_token_offset: handler_ix_layout::DATA_TARGET_TOKEN_OFFSET,
        data_action_idxs_offset: handler_ix_layout::DATA_ACTION_IDXS_OFFSET,
    };

    verify_ix_routing_payload(
        &prev_ix,
        &layout,
        expected,
        SraError::InvalidPrecedingInstruction,
    )
}
