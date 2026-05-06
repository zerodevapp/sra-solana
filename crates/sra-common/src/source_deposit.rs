use anchor_lang::prelude::*;

use crate::abi::u256_from_u64;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedDepositIntent {
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
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
            target_chain_id: params.target_chain_id,
            target_handler: params.target_handler,
            target_adapter: params.target_adapter,
            recipient: params.recipient,
            app_id: params.app_id,
            target_token: params.target_token,
            action_idxs: params.action_idxs.clone(),
        }
    }

    pub fn from_across(params: &AcrossDepositParams) -> Self {
        Self {
            input_amount: params.input_amount,
            output_amount: params.output_amount,
            target_chain_id: params.target_chain_id,
            target_handler: params.target_handler,
            target_adapter: params.target_adapter,
            recipient: params.recipient,
            app_id: params.app_id,
            target_token: params.target_token,
            action_idxs: params.action_idxs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evm_bytes32(byte: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[12..].fill(byte);
        out
    }

    #[test]
    fn handler_output_is_promoted_to_u256() {
        assert_eq!(handler_output_amount_u256(0x1234)[30..], [0x12, 0x34]);
    }

    #[test]
    fn normalized_intent_compares_semantic_fields() {
        let prepare = PrepareParams::new(
            11,
            22,
            42_161,
            evm_bytes32(0x11),
            evm_bytes32(0x22),
            evm_bytes32(0x33),
            [0x44u8; 32],
            evm_bytes32(0x77),
            vec![3, 5],
        );
        let across =
            AcrossDepositParams::from_prepare(&prepare, Pubkey::new_from_array([1u8; 32]), 60, 30);

        assert_eq!(
            NormalizedDepositIntent::from_prepare(&prepare),
            NormalizedDepositIntent::from_across(&across)
        );
    }
}
