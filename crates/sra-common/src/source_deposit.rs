use anchor_lang::prelude::*;

use crate::abi::u256_from_u64;
use crate::errors::SraError;
use crate::types::SourceHandlerData;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrepareParams {
    /// Gross deposit; the protocol fee is deducted from this on the source.
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
    pub protocol_fee_amount: u64,
    /// Owner-signed assertion that skips the protocol fee.
    pub is_sponsor: bool,
    /// `NO_FEE_BPS_OVERRIDE` (u16::MAX) = no override.
    pub protocol_fee_bps_override: u16,
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
        protocol_fee_amount: u64,
        is_sponsor: bool,
        protocol_fee_bps_override: u16,
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
            protocol_fee_amount,
            is_sponsor,
            protocol_fee_bps_override,
        }
    }

    /// Amount actually bridged: gross input minus the attested protocol fee
    /// Fails closed when the fee exceeds the input.
    pub fn net_input_amount(&self) -> Result<u64> {
        self.input_amount
            .checked_sub(self.protocol_fee_amount)
            .ok_or_else(|| error!(SraError::ArithmeticOverflow))
    }
}

impl AcrossDepositParams {
    pub fn from_prepare(
        prepare: &PrepareParams,
        exclusive_relayer: Pubkey,
        fill_deadline_offset: u32,
        exclusivity_period: u32,
    ) -> Result<Self> {
        Ok(Self {
            input_amount: prepare.net_input_amount()?,
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
        })
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
    use crate::constants::MIN_FILL_DEADLINE_OFFSET;
    use crate::errors::SraError;
    anchor_lang::require!(
        fill_deadline_offset >= MIN_FILL_DEADLINE_OFFSET,
        SraError::FillDeadlineTooShort
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
    use crate::constants::NO_FEE_BPS_OVERRIDE;

    #[test]
    fn handler_output_is_promoted_to_u256() {
        assert_eq!(handler_output_amount_u256(0x1234)[30..], [0x12, 0x34]);
    }

    fn fee_prepare(input_amount: u64, protocol_fee_amount: u64) -> PrepareParams {
        PrepareParams::new(
            input_amount,
            456,
            42_161,
            [0x11u8; 32],
            [0x22u8; 32],
            [0x33u8; 32],
            [0x44u8; 32],
            [0xABu8; 32],
            vec![1, 2],
            protocol_fee_amount,
            false,
            NO_FEE_BPS_OVERRIDE,
        )
    }

    #[test]
    fn net_input_amount_subtracts_protocol_fee() {
        assert_eq!(fee_prepare(100, 3).net_input_amount().unwrap(), 97);
    }

    #[test]
    fn net_input_amount_zero_fee_is_identity() {
        assert_eq!(fee_prepare(100, 0).net_input_amount().unwrap(), 100);
    }

    #[test]
    fn net_input_amount_underflow_errors() {
        let err = fee_prepare(100, 101)
            .net_input_amount()
            .expect_err("fee above input must not underflow silently");
        match err {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, "ArithmeticOverflow");
            }
            other => panic!("expected AnchorError ArithmeticOverflow, got {other:?}"),
        }
    }

    #[test]
    fn from_prepare_bridges_net_of_protocol_fee() {
        let prepare = fee_prepare(100, 3);
        let deposit = AcrossDepositParams::from_prepare(&prepare, Pubkey::new_unique(), 600, 120)
            .expect("valid fee must convert");
        assert_eq!(deposit.input_amount, 97);
    }

    #[test]
    fn from_prepare_propagates_fee_underflow() {
        let prepare = fee_prepare(100, 101);
        AcrossDepositParams::from_prepare(&prepare, Pubkey::new_unique(), 600, 120)
            .expect_err("fee above input must fail closed");
    }

    #[test]
    fn prepare_params_wire_appends_fee_fields_in_order() {
        // The Borsh tail after `action_idxs` is the cross-program attestation
        // contract: protocol_fee_amount (u64 LE) ‖ is_sponsor (u8) ‖
        // protocol_fee_bps_override (u16 LE). Drift breaks the adapter decode.
        let mut prepare = fee_prepare(100, 0x0102030405060708);
        prepare.is_sponsor = true;
        prepare.protocol_fee_bps_override = 0x1234;
        let bytes = anchor_lang::AnchorSerialize::try_to_vec(&prepare).unwrap();
        let tail = &bytes[bytes.len() - 11..];
        assert_eq!(
            tail,
            [
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // fee u64 LE
                0x01, // is_sponsor
                0x34, 0x12, // override u16 LE
            ]
        );
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
}
