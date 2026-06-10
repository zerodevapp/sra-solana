use anchor_lang::prelude::*;

use crate::abi::u256_from_u64;
use crate::types::SourceHandlerData;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrepareParams {
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
}

/// Wire-format mirror of the `validate_and_prepare` instruction args. Handler
/// and adapter MUST agree on this layout byte-for-byte; sharing one definition
/// here keeps them coupled. Field order matches the order of the handler's
/// IX args in `programs/sra-handler/src/lib.rs::validate_and_prepare`.
#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct ValidateAndPrepareArgs {
    pub handler_data: SourceHandlerData,
    pub params: PrepareParams,
    pub paired_adapter_ix_data_hash: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct AcrossDepositParams {
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
}

impl PrepareParams {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_amount: u64,
        output_amount: u64,
        target_chain_id: u64,
        target_handler: [u8; 32],
        target_adapter: [u8; 32],
        recipient: [u8; 32],
        app_id: [u8; 32],
        target_token: [u8; 32],
        action_idxs: Vec<u64>,
    ) -> Self {
        Self {
            input_amount,
            output_amount,
            target_chain_id,
            target_handler,
            target_adapter,
            recipient,
            app_id,
            target_token,
            action_idxs,
        }
    }
}

impl AcrossDepositParams {
    pub fn from_prepare(
        prepare: &PrepareParams,
        exclusive_relayer: Pubkey,
        fill_deadline_offset: u32,
        exclusivity_period: u32,
    ) -> Self {
        Self {
            input_amount: prepare.input_amount,
            output_amount: handler_output_amount_u256(prepare.output_amount),
            target_chain_id: prepare.target_chain_id,
            target_handler: prepare.target_handler,
            target_adapter: prepare.target_adapter,
            recipient: prepare.recipient,
            app_id: prepare.app_id,
            target_token: prepare.target_token,
            action_idxs: prepare.action_idxs.clone(),
            exclusive_relayer,
            fill_deadline_offset,
            exclusivity_period,
        }
    }
}

pub fn handler_output_amount_u256(output_amount: u64) -> [u8; 32] {
    u256_from_u64(output_amount)
}

/// Bound user-supplied Across timing: a too-short `fill_deadline_offset` forces
/// instant expiry and strands the refund in `adapter_custody`.
pub fn validate_across_timing(
    fill_deadline_offset: u32,
    exclusivity_period: u32,
) -> anchor_lang::Result<()> {
    use crate::constants::{MAX_FILL_DEADLINE_OFFSET, MIN_FILL_DEADLINE_OFFSET};
    use crate::errors::SraError;
    anchor_lang::require!(
        fill_deadline_offset >= MIN_FILL_DEADLINE_OFFSET,
        SraError::FillDeadlineTooShort
    );
    anchor_lang::require!(
        fill_deadline_offset <= MAX_FILL_DEADLINE_OFFSET,
        SraError::FillDeadlineTooLong
    );
    anchor_lang::require!(
        exclusivity_period <= fill_deadline_offset,
        SraError::ExclusivityExceedsDeadline
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::SraError;

    #[test]
    fn handler_output_is_promoted_to_u256() {
        assert_eq!(handler_output_amount_u256(0x1234)[30..], [0x12, 0x34]);
    }

    fn assert_timing_err(result: anchor_lang::Result<()>, expected: &str) {
        match result.unwrap_err() {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, expected);
            }
            other => panic!("expected AnchorError {expected}, got {other:?}"),
        }
    }

    #[test]
    fn validate_across_timing_rejects_too_short_deadline() {
        assert_timing_err(validate_across_timing(299, 0), "FillDeadlineTooShort");
    }

    #[test]
    fn validate_across_timing_accepts_min_deadline() {
        validate_across_timing(300, 0).expect("min deadline must be accepted");
    }

    #[test]
    fn validate_across_timing_accepts_max_deadline() {
        validate_across_timing(21_600, 0).expect("max deadline must be accepted");
    }

    #[test]
    fn validate_across_timing_rejects_too_long_deadline() {
        assert_timing_err(validate_across_timing(21_601, 0), "FillDeadlineTooLong");
    }

    #[test]
    fn validate_across_timing_accepts_exclusivity_equal_to_deadline() {
        validate_across_timing(300, 300).expect("exclusivity == deadline must be accepted");
    }

    #[test]
    fn validate_across_timing_rejects_exclusivity_above_deadline() {
        assert_timing_err(
            validate_across_timing(300, 301),
            "ExclusivityExceedsDeadline",
        );
    }

    // Keep the linter aware these are referenced from production code paths.
    const _: fn(u32, u32) -> anchor_lang::Result<()> = validate_across_timing;
    const _: SraError = SraError::FillDeadlineTooShort;
}
