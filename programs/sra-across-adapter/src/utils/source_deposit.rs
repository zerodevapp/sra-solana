use anchor_lang::{error, prelude::*};
use sra_common::{
    abi::{
        abi_encode_sra_message, build_extra_envelope, bytes32_to_evm_address, source_token_hash,
        u256_from_u64, DepositInfo, Message, TokenParam,
    },
    constants::SOLANA_CHAIN_ID,
    errors::SraError,
    source_deposit::{AcrossDepositParams, PrepareParams},
    types::SraConfig,
};

use crate::utils::seed_hash::{deposit_now_delegate, DelegateDerivation, DepositNowParams};

pub struct DepositNowBuild {
    pub params: DepositNowParams,
    pub delegate: DelegateDerivation,
}

fn narrow_evm_address(value: &[u8; 32]) -> Result<[u8; 20]> {
    bytes32_to_evm_address(value).map_err(|_| error!(SraError::InvalidEvmAddress))
}

pub fn build_deposit_now_bundle(
    adapter_authority: &Pubkey,
    sra_config: &SraConfig,
    input_mint: &Pubkey,
    prepare: &PrepareParams,
    deposit: &AcrossDepositParams,
    prepared_nonce: u64,
) -> Result<DepositNowBuild> {
    let target_handler = narrow_evm_address(&sra_config.target_handler)?;
    let target_adapter = narrow_evm_address(&sra_config.target_adapter)?;
    let target_token = narrow_evm_address(&prepare.target_token)?;

    let message = Message {
        account: sra_config.recipient,
        init_code: Vec::new(),
        deposit_info: DepositInfo {
            deposit_param: TokenParam {
                token: source_token_hash(input_mint),
                amount: u256_from_u64(prepare.input_amount),
            },
            target_chain_id: u256_from_u64(sra_config.target_chain_id),
            target_param: TokenParam {
                token: target_token,
                amount: u256_from_u64(prepare.output_amount),
            },
            extra: build_extra_envelope(&target_adapter, prepared_nonce),
        },
        action_idxs: prepare
            .action_idxs
            .iter()
            .copied()
            .map(u256_from_u64)
            .collect(),
        app_id: sra_config.app_id,
    };
    let canonical_message = abi_encode_sra_message(
        &target_adapter,
        &target_handler,
        &u256_from_u64(SOLANA_CHAIN_ID),
        &message,
    );

    let params = DepositNowParams {
        depositor: *adapter_authority,
        recipient: Pubkey::new_from_array(sra_config.target_adapter),
        input_token: *input_mint,
        output_token: Pubkey::new_from_array(prepare.target_token),
        input_amount: prepare.input_amount,
        output_amount: u256_from_u64(prepare.output_amount),
        destination_chain_id: sra_config.target_chain_id,
        exclusive_relayer: deposit.exclusive_relayer,
        fill_deadline_offset: deposit.fill_deadline_offset,
        exclusivity_period: deposit.exclusivity_period,
        message: canonical_message,
    };
    let delegate = deposit_now_delegate(&sra_common::constants::ACROSS_SPOKE_POOL_ID, &params)?;

    Ok(DepositNowBuild { params, delegate })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sra_common::source_deposit::PrepareParams;

    fn evm_bytes32(byte: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[12..].fill(byte);
        out
    }

    fn sample_sra_config() -> SraConfig {
        SraConfig {
            owner: Pubkey::new_unique(),
            target_chain_id: 42161,
            target_handler: evm_bytes32(0x11),
            target_adapter: evm_bytes32(0x22),
            recipient: evm_bytes32(0x33),
            app_id: [0x44u8; 32],
            salt: [0x55u8; 32],
            allowed_adapters: vec![],
            allowed_slippage_bps: 50,
            bump: 254,
        }
    }

    #[test]
    fn build_deposit_now_bundle_uses_prepare_intent_fields() {
        let adapter_authority = Pubkey::new_unique();
        let input_mint = Pubkey::new_unique();
        let prepare = PrepareParams {
            input_amount: 11,
            output_amount: 22,
            target_token: evm_bytes32(0xbb),
            action_idxs: vec![1, 2],
        };
        let deposit = AcrossDepositParams {
            input_amount: 99,
            output_amount: u256_from_u64(88),
            target_token: evm_bytes32(0xaa),
            action_idxs: vec![9, 10],
            exclusive_relayer: Pubkey::new_unique(),
            fill_deadline_offset: 600,
            exclusivity_period: 120,
        };
        let matching_deposit = AcrossDepositParams {
            input_amount: prepare.input_amount,
            output_amount: u256_from_u64(prepare.output_amount),
            target_token: prepare.target_token,
            action_idxs: prepare.action_idxs.clone(),
            exclusive_relayer: deposit.exclusive_relayer,
            fill_deadline_offset: deposit.fill_deadline_offset,
            exclusivity_period: deposit.exclusivity_period,
        };

        let build = build_deposit_now_bundle(
            &adapter_authority,
            &sample_sra_config(),
            &input_mint,
            &prepare,
            &deposit,
            7,
        )
        .unwrap();
        let matching = build_deposit_now_bundle(
            &adapter_authority,
            &sample_sra_config(),
            &input_mint,
            &prepare,
            &matching_deposit,
            7,
        )
        .unwrap();

        assert_eq!(build.params.input_amount, prepare.input_amount);
        assert_eq!(
            build.params.output_amount,
            u256_from_u64(prepare.output_amount)
        );
        assert_eq!(
            build.params.output_token,
            Pubkey::new_from_array(prepare.target_token)
        );
        assert_eq!(build.params.message, matching.params.message);
    }
}
