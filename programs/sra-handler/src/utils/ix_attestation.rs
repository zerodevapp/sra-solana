//! Handler-side wrapper for the bidirectional IX attestation defined in
//! `sra_common::ix_attestation`. `validate_and_prepare` calls
//! `verify_adapter_ix` to confirm IX 2 in the same versioned transaction is
//! `sra_across_adapter::execute_deposit_across` for this SRA, with byte-
//! identical routing fields. The pure comparison logic and the unit tests
//! live in sra-common; this wrapper only loads the next instruction from the
//! Instructions sysvar and forwards it.

use anchor_lang::prelude::*;
use sra_common::{
    adapter_ix_layout,
    errors::SraError,
    ix_attestation::{verify_ix_routing_payload, IxLayout, Routing},
};

/// Confirm the immediately-following instruction is the registered adapter's
/// `execute_deposit_across` for `expected_sra_config`, and that it forwards
/// the same `expected` routing payload IX 1 just pinned.
///
/// Returns `SraError::InvalidFollowingInstruction` on any of:
/// - no instruction at relative offset +1 (standalone IX 1);
/// - IX 2 program id does not match `expected_adapter_program`;
/// - IX 2 discriminator is not `EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR`;
/// - IX 2's `sra_config` slot does not match `expected_sra_config`;
/// - any of the routing fields (`input_amount`, `output_amount`,
///   `target_token`, `action_idxs`) differ from `expected`.
pub fn verify_adapter_ix(
    instructions_sysvar: &AccountInfo,
    expected_adapter_program: &Pubkey,
    expected_sra_config: &Pubkey,
    expected: &Routing,
) -> Result<()> {
    // The deprecation here is on the anchor re-export, not on the underlying
    // function. Switching the dep over is a Step 45 cleanup task — for now
    // suppress the warning at the use site.
    #[allow(deprecated)]
    let next_ix = anchor_lang::solana_program::sysvar::instructions::get_instruction_relative(
        1,
        instructions_sysvar,
    )
    .map_err(|_| error!(SraError::InvalidFollowingInstruction))?;

    let layout = IxLayout {
        expected_program: expected_adapter_program,
        expected_discriminator: &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
        expected_sra_config,
        sra_config_account_index: adapter_ix_layout::ACCT_SRA_CONFIG,
        data_input_amount_offset: adapter_ix_layout::DATA_INPUT_AMOUNT_OFFSET,
        data_output_amount_offset: adapter_ix_layout::DATA_OUTPUT_AMOUNT_OFFSET,
        data_target_token_offset: adapter_ix_layout::DATA_TARGET_TOKEN_OFFSET,
        data_action_idxs_offset: adapter_ix_layout::DATA_ACTION_IDXS_OFFSET,
    };

    verify_ix_routing_payload(
        &next_ix,
        &layout,
        expected,
        SraError::InvalidFollowingInstruction,
    )
}
