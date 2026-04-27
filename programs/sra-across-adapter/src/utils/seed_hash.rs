use anchor_lang::{error, prelude::*};
use solana_program::keccak::hash as keccak_hash;

use crate::errors::AcrossAdapterError;

/// Seed prefix the Across SVM Spoke program uses for delegate signer PDAs.
pub const DELEGATE_SEED_PREFIX: &[u8] = b"delegate";
const DEPOSIT_NOW_FIXED_SERIALIZED_LEN: usize = (32 * 4) + 8 + 32 + 8 + 32 + 4 + 4 + 4;

/// Deposit-now inputs that participate in Across's delegate seed hash.
#[derive(AnchorSerialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositNowParams {
    pub depositor: Pubkey,
    pub recipient: Pubkey,
    pub input_token: Pubkey,
    pub output_token: Pubkey,
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub destination_chain_id: u64,
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
    pub message: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelegateDerivation {
    pub seed_hash: [u8; 32],
    pub pda: Pubkey,
    pub bump: u8,
}

/// `keccak256(anchor_serialize(deposit_now_args))`, matching Across's SVM Spoke.
pub fn deposit_now_seed_hash(params: &DepositNowParams) -> Result<[u8; 32]> {
    let mut serialized =
        Vec::with_capacity(DEPOSIT_NOW_FIXED_SERIALIZED_LEN + params.message.len());
    params
        .serialize(&mut serialized)
        .map_err(|_| error!(AcrossAdapterError::DepositNowSeedSerializationFailed))?;
    Ok(keccak_hash(&serialized).to_bytes())
}

/// Across delegates token movement to a PDA derived as `["delegate", seed_hash]`.
pub fn delegate_pda(program_id: &Pubkey, seed_hash: [u8; 32]) -> DelegateDerivation {
    let (pda, bump) =
        Pubkey::find_program_address(&[DELEGATE_SEED_PREFIX, seed_hash.as_ref()], program_id);
    DelegateDerivation {
        seed_hash,
        pda,
        bump,
    }
}

/// Convenience helper for the exact delegate signer needed by `deposit_now(...)`.
pub fn deposit_now_delegate(
    program_id: &Pubkey,
    params: &DepositNowParams,
) -> Result<DelegateDerivation> {
    Ok(delegate_pda(program_id, deposit_now_seed_hash(params)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sra_common::{abi::u256_from_u64, constants::ACROSS_SPOKE_POOL_ID};
    use std::str::FromStr;

    fn sample_params() -> DepositNowParams {
        DepositNowParams {
            depositor: Pubkey::new_from_array([1u8; 32]),
            recipient: Pubkey::new_from_array([2u8; 32]),
            input_token: Pubkey::new_from_array([3u8; 32]),
            output_token: Pubkey::new_from_array([4u8; 32]),
            input_amount: 10_000_000,
            output_amount: u256_from_u64(9_998_500),
            destination_chain_id: 8_453,
            exclusive_relayer: Pubkey::new_from_array([5u8; 32]),
            fill_deadline_offset: 3_600,
            exclusivity_period: 900,
            message: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    fn manual_seed_preimage(params: &DepositNowParams) -> Vec<u8> {
        let mut out = Vec::with_capacity(DEPOSIT_NOW_FIXED_SERIALIZED_LEN + params.message.len());
        out.extend_from_slice(params.depositor.as_ref());
        out.extend_from_slice(params.recipient.as_ref());
        out.extend_from_slice(params.input_token.as_ref());
        out.extend_from_slice(params.output_token.as_ref());
        out.extend_from_slice(&params.input_amount.to_le_bytes());
        out.extend_from_slice(&params.output_amount);
        out.extend_from_slice(&params.destination_chain_id.to_le_bytes());
        out.extend_from_slice(params.exclusive_relayer.as_ref());
        out.extend_from_slice(&params.fill_deadline_offset.to_le_bytes());
        out.extend_from_slice(&params.exclusivity_period.to_le_bytes());
        out.extend_from_slice(&(params.message.len() as u32).to_le_bytes());
        out.extend_from_slice(&params.message);
        out
    }

    fn encode_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    #[test]
    fn compute_seed_hash_deterministic() {
        let params = sample_params();
        assert_eq!(
            deposit_now_seed_hash(&params).unwrap(),
            deposit_now_seed_hash(&params).unwrap()
        );
    }

    #[test]
    fn deposit_now_seed_hash_matches_across_layout() {
        let params = sample_params();
        let expected = keccak_hash(&manual_seed_preimage(&params)).to_bytes();
        let derived = deposit_now_seed_hash(&params).unwrap();
        assert_eq!(derived, expected);
        assert_eq!(
            encode_hex(&derived),
            "2f78ae0f689327fc3e04359f81a12db6c0e105259be0ee7db75e6b060d8f49f1"
        );
    }

    #[test]
    fn derive_delegate_pda_matches_across_vector() {
        let params = sample_params();
        let derived = deposit_now_delegate(&ACROSS_SPOKE_POOL_ID, &params).unwrap();
        assert_eq!(derived.seed_hash, deposit_now_seed_hash(&params).unwrap());
        assert_eq!(
            derived.pda,
            Pubkey::from_str("DXnWShwq4fnWimLiwBNbQG1UAVaCL1pWX4SeV9y5HrjD").unwrap()
        );
        assert_eq!(derived.bump, 255);
    }

    #[test]
    fn deposit_now_seed_hash_changes_with_inputs() {
        let mut params = sample_params();
        let initial = deposit_now_seed_hash(&params).unwrap();

        params.fill_deadline_offset += 1;
        assert_ne!(deposit_now_seed_hash(&params).unwrap(), initial);
    }
}
