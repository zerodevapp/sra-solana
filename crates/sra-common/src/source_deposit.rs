use anchor_lang::prelude::*;

use crate::abi::u256_from_u64;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrepareParams {
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct AcrossDepositParams {
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
}

impl PrepareParams {
    pub fn new(
        input_amount: u64,
        output_amount: u64,
        target_token: [u8; 32],
        action_idxs: Vec<u64>,
    ) -> Self {
        Self {
            input_amount,
            output_amount,
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
            target_token: prepare.target_token,
            action_idxs: prepare.action_idxs.clone(),
            exclusive_relayer,
            fill_deadline_offset,
            exclusivity_period,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedDepositIntent {
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_token: [u8; 32],
    pub action_idxs: Vec<u64>,
}

pub fn handler_output_amount_u256(output_amount: u64) -> [u8; 32] {
    u256_from_u64(output_amount)
}

impl NormalizedDepositIntent {
    pub fn from_prepare(params: &PrepareParams) -> Self {
        Self {
            input_amount: params.input_amount,
            output_amount: handler_output_amount_u256(params.output_amount),
            target_token: params.target_token,
            action_idxs: params.action_idxs.clone(),
        }
    }

    pub fn from_across(params: &AcrossDepositParams) -> Self {
        Self {
            input_amount: params.input_amount,
            output_amount: params.output_amount,
            target_token: params.target_token,
            action_idxs: params.action_idxs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_output_is_promoted_to_u256() {
        assert_eq!(handler_output_amount_u256(0x1234)[30..], [0x12, 0x34]);
    }

    #[test]
    fn normalized_intent_compares_semantic_fields() {
        let prepare = PrepareParams::new(11, 22, [7u8; 32], vec![3, 5]);
        let across =
            AcrossDepositParams::from_prepare(&prepare, Pubkey::new_from_array([1u8; 32]), 60, 30);

        assert_eq!(
            NormalizedDepositIntent::from_prepare(&prepare),
            NormalizedDepositIntent::from_across(&across)
        );
    }
}
