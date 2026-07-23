use anchor_lang::prelude::*;
use sra_common::{
    source_deposit::{AcrossDepositParams, PrepareParams},
    types::SraConfig,
};

use crate::utils::seed_hash::{deposit_now_delegate, DelegateDerivation, DepositNowParams};

pub struct DepositNowBuild {
    pub params: DepositNowParams,
    pub delegate: DelegateDerivation,
}

/// Builds the Across `deposit_now` bundle for the adapter leg of a paired
/// source deposit.
///
/// The two-transaction target-execution model carries no cross-chain envelope,
/// so the bridged `message` is always empty bytes — mirroring the EVM
/// `AcrossAdapter`, which calls `depositV3` with `message = ""`. The
/// destination runs its hash-committed actions from a separate instruction, so
/// nothing rides in the bridge payload.
pub fn build_deposit_now_bundle(
    adapter_authority: &Pubkey,
    _sra_config: &SraConfig,
    input_mint: &Pubkey,
    prepare: &PrepareParams,
    deposit: &AcrossDepositParams,
) -> Result<DepositNowBuild> {
    let bridged_amount = prepare.net_input_amount()?;

    let params = DepositNowParams {
        depositor: *adapter_authority,
        recipient: Pubkey::new_from_array(prepare.recipient),
        input_token: *input_mint,
        output_token: Pubkey::new_from_array(prepare.target_token),
        input_amount: bridged_amount,
        output_amount: prepare.output_amount,
        destination_chain_id: prepare.target_chain_id,
        exclusive_relayer: deposit.exclusive_relayer,
        fill_deadline_offset: deposit.fill_deadline_offset,
        exclusivity_period: deposit.exclusivity_period,
        message: Vec::new(),
    };
    let delegate = deposit_now_delegate(&sra_common::constants::ACROSS_SPOKE_POOL_ID, &params)?;

    Ok(DepositNowBuild { params, delegate })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sra_common::abi::u256_from_u64;
    use sra_common::constants::NO_FEE_BPS_OVERRIDE;
    use sra_common::source_deposit::PrepareParams;

    fn evm_bytes32(byte: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[12..].fill(byte);
        out
    }

    fn sample_sra_config() -> SraConfig {
        SraConfig {
            owner: Pubkey::new_unique(),
            final_handler: Pubkey::new_unique(),
            handler_data_hash: [0x11u8; 32],
            salt: [0x55u8; 32],
            bump: 254,
        }
    }

    #[test]
    fn build_deposit_now_bundle_bridges_net_with_empty_message() {
        let prepare = PrepareParams::new(
            11,
            u256_from_u64(22),
            42_161,
            evm_bytes32(0x33),
            evm_bytes32(0xbb),
            2,
            false,
            NO_FEE_BPS_OVERRIDE,
        );
        let deposit = AcrossDepositParams::from_prepare(&prepare, Pubkey::new_unique(), 600, 120)
            .expect("valid fee must convert");

        let build = build_deposit_now_bundle(
            &Pubkey::new_unique(),
            &sample_sra_config(),
            &Pubkey::new_unique(),
            &prepare,
            &deposit,
        )
        .unwrap();

        assert_eq!(
            build.params.input_amount, 9,
            "Across pulls the net (gross 11 - fee 2)"
        );
        assert!(
            build.params.message.is_empty(),
            "the two-tx model bridges an empty Across message (EVM parity)",
        );
    }

    #[test]
    fn build_deposit_now_bundle_uses_prepare_intent_fields() {
        let adapter_authority = Pubkey::new_unique();
        let input_mint = Pubkey::new_unique();
        let prepare = PrepareParams::new(
            11,
            u256_from_u64(22),
            42_161,
            evm_bytes32(0x33),
            evm_bytes32(0xbb),
            0,
            false,
            NO_FEE_BPS_OVERRIDE,
        );
        let deposit = AcrossDepositParams::from_prepare(&prepare, Pubkey::new_unique(), 600, 120)
            .expect("sample params must produce a valid net amount");

        let build = build_deposit_now_bundle(
            &adapter_authority,
            &sample_sra_config(),
            &input_mint,
            &prepare,
            &deposit,
        )
        .unwrap();

        assert_eq!(build.params.depositor, adapter_authority);
        assert_eq!(build.params.input_token, input_mint);
        // Fee 0 ⇒ net == gross.
        assert_eq!(build.params.input_amount, prepare.input_amount);
        assert_eq!(build.params.output_amount, prepare.output_amount);
        assert_eq!(
            build.params.output_token,
            Pubkey::new_from_array(prepare.target_token)
        );
        assert_eq!(build.params.destination_chain_id, prepare.target_chain_id);
        assert_eq!(
            build.params.recipient,
            Pubkey::new_from_array(prepare.recipient)
        );
        assert_eq!(build.params.exclusive_relayer, deposit.exclusive_relayer);
        assert_eq!(
            build.params.fill_deadline_offset,
            deposit.fill_deadline_offset
        );
        assert_eq!(build.params.exclusivity_period, deposit.exclusivity_period);
        assert!(build.params.message.is_empty());
        // The delegate is derived deterministically from the (empty-message)
        // deposit params, matching Across's SVM Spoke seed scheme.
        assert_eq!(
            build.delegate,
            deposit_now_delegate(&sra_common::constants::ACROSS_SPOKE_POOL_ID, &build.params)
                .unwrap()
        );
    }
}
