use anchor_lang::prelude::*;

use crate::constants::BPS_DENOMINATOR;
use crate::errors::SraError;
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrepareParams {
    /// Gross deposit; the protocol fee is deducted from this on the source.
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub recipient: [u8; 32],
    pub target_token: [u8; 32],
    pub protocol_fee_amount: u64,
    /// Initiator-supplied assertion that skips the protocol fee; honored only
    /// when the fee admin enabled `allow_sponsored`, else `SponsoredNotAllowed`.
    pub is_sponsor: bool,
    /// `NO_FEE_BPS_OVERRIDE` (u16::MAX) = no override.
    pub protocol_fee_bps_override: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct TriggerArgs {
    pub params: PrepareParams,
    pub paired_adapter_ix_data_hash: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct AcrossDepositParams {
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub recipient: [u8; 32],
    pub target_token: [u8; 32],
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
}

impl PrepareParams {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_amount: u64,
        output_amount: [u8; 32],
        target_chain_id: u64,
        recipient: [u8; 32],
        target_token: [u8; 32],
        protocol_fee_amount: u64,
        is_sponsor: bool,
        protocol_fee_bps_override: u16,
    ) -> Self {
        Self {
            input_amount,
            output_amount,
            target_chain_id,
            recipient,
            target_token,
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
            output_amount: prepare.output_amount,
            target_chain_id: prepare.target_chain_id,
            recipient: prepare.recipient,
            target_token: prepare.target_token,
            exclusive_relayer,
            fill_deadline_offset,
            exclusivity_period,
        })
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CctpDepositParams {
    /// Net of protocol fee (`PrepareParams::net_input_amount`).
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub recipient: [u8; 32],
    pub target_token: [u8; 32],
    /// CCTP fee cap; the destination-side `fee_executed` is bounded by this.
    pub max_fee: u64,
    /// CCTP v2 finality; canonical values are `FINALITY_THRESHOLD_FAST`
    /// (1000) and `FINALITY_THRESHOLD_FINALIZED` (2000), and the adapter
    /// accepts only those two values.
    pub min_finality_threshold: u32,
}

impl CctpDepositParams {
    pub fn from_prepare(
        prepare: &PrepareParams,
        max_fee: u64,
        min_finality_threshold: u32,
    ) -> Result<Self> {
        Ok(Self {
            input_amount: prepare.net_input_amount()?,
            output_amount: prepare.output_amount,
            target_chain_id: prepare.target_chain_id,
            recipient: prepare.recipient,
            target_token: prepare.target_token,
            max_fee,
            min_finality_threshold,
        })
    }
}

pub fn decode_output_amount(output_amount: [u8; 32]) -> Result<u128> {
    require!(
        output_amount[..16] == [0u8; 16],
        SraError::OutputAmountOverflow
    );
    Ok(u128::from_be_bytes(output_amount[16..].try_into().unwrap()))
}

pub fn output_satisfies_slippage_floor(
    input_amount: u64,
    input_decimals: u8,
    output_amount: u128,
    output_decimals: u8,
    slippage_bps: u16,
) -> Result<bool> {
    let denominator = BPS_DENOMINATOR as u128;
    let slippage_numerator = (BPS_DENOMINATOR - slippage_bps) as u128;
    let minimum_source = (input_amount as u128)
        .checked_mul(slippage_numerator)
        .ok_or(SraError::ArithmeticOverflow)?
        .div_ceil(denominator);
    if input_decimals >= output_decimals {
        let exponent = (input_decimals - output_decimals) as u32;
        let output = 10u128
            .checked_pow(exponent)
            .and_then(|scale| output_amount.checked_mul(scale));
        Ok(output
            .map(|scaled| scaled >= minimum_source)
            .unwrap_or(output_amount > 0 || minimum_source == 0))
    } else {
        let exponent = (output_decimals - input_decimals) as u32;
        let minimum = 10u128
            .checked_pow(exponent)
            .and_then(|scale| minimum_source.checked_mul(scale));
        Ok(minimum
            .map(|scaled| output_amount >= scaled)
            .unwrap_or(minimum_source == 0))
    }
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
    use crate::abi::u256_from_u64;
    use crate::constants::NO_FEE_BPS_OVERRIDE;

    #[test]
    fn decode_output_amount_accepts_u64_max() {
        assert_eq!(
            decode_output_amount(u256_from_u64(u64::MAX)).unwrap(),
            u64::MAX as u128
        );
    }

    #[test]
    fn decode_output_amount_accepts_above_u64_max() {
        let mut wire = [0u8; 32];
        wire[16..].copy_from_slice(&(1u128 << 64).to_be_bytes());
        assert_eq!(decode_output_amount(wire).unwrap(), 1u128 << 64);
    }

    #[test]
    fn decode_output_amount_accepts_u128_max() {
        let mut wire = [0u8; 32];
        wire[16..].copy_from_slice(&u128::MAX.to_be_bytes());
        assert_eq!(decode_output_amount(wire).unwrap(), u128::MAX);
    }

    #[test]
    fn decode_output_amount_rejects_upper_bytes() {
        let mut wire = [0u8; 32];
        wire[15] = 1;
        match decode_output_amount(wire).expect_err("upper bytes must reject") {
            anchor_lang::error::Error::AnchorError(e) => {
                assert_eq!(e.error_name, "OutputAmountOverflow")
            }
            other => panic!("expected OutputAmountOverflow, got {other:?}"),
        }
    }

    #[test]
    fn slippage_floor_rounds_up_at_equal_decimals() {
        assert!(!output_satisfies_slippage_floor(1_000_001, 6, 995_000, 6, 50).unwrap());
        assert!(output_satisfies_slippage_floor(1_000_001, 6, 995_001, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_rounds_up_when_target_is_more_precise() {
        assert!(!output_satisfies_slippage_floor(1_000_001, 6, 9_950_009, 7, 50).unwrap());
        assert!(output_satisfies_slippage_floor(1_000_001, 6, 9_950_010, 7, 50).unwrap());
    }

    #[test]
    fn slippage_floor_rounds_up_when_source_is_more_precise() {
        assert!(!output_satisfies_slippage_floor(10_051, 7, 1_000, 6, 50).unwrap());
        assert!(output_satisfies_slippage_floor(10_051, 7, 1_001, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_rejects_unrepresentable_target_scale() {
        assert!(!output_satisfies_slippage_floor(1, 6, u128::MAX, 45, 50).unwrap());
    }

    #[test]
    fn slippage_floor_accepts_unrepresentable_positive_output_scale() {
        assert!(output_satisfies_slippage_floor(1, 45, 1, 6, 50).unwrap());
        assert!(!output_satisfies_slippage_floor(1, 45, 0, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_zero_minimum_survives_unrepresentable_scales() {
        assert!(output_satisfies_slippage_floor(1, 6, 0, 45, 10_000).unwrap());
        assert!(output_satisfies_slippage_floor(1, 45, 0, 6, 10_000).unwrap());
    }

    #[test]
    fn slippage_floor_exact_division_boundary_unchanged() {
        assert!(output_satisfies_slippage_floor(1_000_000, 6, 995_000, 6, 50).unwrap());
        assert!(!output_satisfies_slippage_floor(1_000_000, 6, 994_999, 6, 50).unwrap());
    }

    #[test]
    fn slippage_floor_zero_slippage_requires_full_amount() {
        assert!(output_satisfies_slippage_floor(1_000_000, 6, 1_000_000, 6, 0).unwrap());
        assert!(!output_satisfies_slippage_floor(1_000_000, 6, 999_999, 6, 0).unwrap());
    }

    fn fee_prepare(input_amount: u64, protocol_fee_amount: u64) -> PrepareParams {
        PrepareParams::new(
            input_amount,
            u256_from_u64(456),
            42_161,
            [0x33u8; 32],
            [0xABu8; 32],
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
    fn cctp_from_prepare_bridges_net_of_protocol_fee() {
        let prepare = fee_prepare(100, 3);
        let deposit =
            CctpDepositParams::from_prepare(&prepare, 2, 1000).expect("valid fee must convert");
        assert_eq!(deposit.input_amount, 97);
    }

    #[test]
    fn cctp_from_prepare_propagates_fee_underflow() {
        let prepare = fee_prepare(100, 101);
        CctpDepositParams::from_prepare(&prepare, 2, 1000)
            .expect_err("fee above input must fail closed");
    }

    #[test]
    fn cctp_deposit_params_wire_layout_is_pinned() {
        // The Borsh layout is the cross-program attestation contract: the
        // handler hashes the adapter's ix data and the adapter re-encodes this
        // struct to compare. Pin every field's offset and endianness.
        let deposit = CctpDepositParams {
            input_amount: 0x0102030405060708,
            output_amount: [0xAA; 32],
            target_chain_id: 0x1112131415161718,
            recipient: [0xBB; 32],
            target_token: [0xCC; 32],
            max_fee: 0x2122232425262728,
            min_finality_threshold: 0x3132_3334,
        };
        let bytes = borsh::to_vec(&deposit).unwrap();
        assert_eq!(bytes.len(), 124);
        assert_eq!(bytes[..8], [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]); // input u64 LE
        assert_eq!(bytes[8..40], [0xAA; 32]); // output_amount
        assert_eq!(
            bytes[40..48],
            [0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11] // chain id u64 LE
        );
        assert_eq!(bytes[48..80], [0xBB; 32]); // recipient
        assert_eq!(bytes[80..112], [0xCC; 32]); // target_token
        assert_eq!(
            bytes[112..120],
            [0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21] // max_fee u64 LE
        );
        assert_eq!(bytes[120..], [0x34, 0x33, 0x32, 0x31]); // threshold u32 LE
    }

    #[test]
    fn prepare_params_wire_appends_fee_fields_in_order() {
        // The Borsh tail after `target_token` is the cross-program attestation
        // contract: protocol_fee_amount (u64 LE) ‖ is_sponsor (u8) ‖
        // protocol_fee_bps_override (u16 LE). Drift breaks the adapter decode.
        let mut prepare = fee_prepare(100, 0x0102030405060708);
        prepare.is_sponsor = true;
        prepare.protocol_fee_bps_override = 0x1234;
        let bytes = borsh::to_vec(&prepare).unwrap();
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
