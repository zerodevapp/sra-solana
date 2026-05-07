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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_output_is_promoted_to_u256() {
        assert_eq!(handler_output_amount_u256(0x1234)[30..], [0x12, 0x34]);
    }
}
