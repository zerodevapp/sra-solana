use anchor_lang::prelude::*;
use solana_program::keccak::hash as keccak_hash;

/// Seed prefix the Across SVM Spoke program uses for delegate signer PDAs.
pub const DELEGATE_SEED_PREFIX: &[u8] = b"delegate";

/// Deposit-now inputs that participate in Across's delegate seed hash.
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Exact Borsh/Anchor layout consumed by Across's `deposit_now(...)` seed hash.
#[derive(AnchorSerialize)]
struct DepositNowSeedData<'a> {
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
    pub message: &'a Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelegateDerivation {
    pub seed_hash: [u8; 32],
    pub pda: Pubkey,
    pub bump: u8,
}

/// `keccak256(anchor_serialize(deposit_now_args))`, matching Across's SVM Spoke.
pub fn deposit_now_seed_hash(params: &DepositNowParams) -> [u8; 32] {
    let seed_data = DepositNowSeedData {
        depositor: params.depositor,
        recipient: params.recipient,
        input_token: params.input_token,
        output_token: params.output_token,
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        destination_chain_id: params.destination_chain_id,
        exclusive_relayer: params.exclusive_relayer,
        fill_deadline_offset: params.fill_deadline_offset,
        exclusivity_period: params.exclusivity_period,
        message: &params.message,
    };

    let mut serialized = Vec::new();
    seed_data
        .serialize(&mut serialized)
        .expect("serializing deposit-now seed data cannot fail");
    keccak_hash(&serialized).to_bytes()
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
pub fn deposit_now_delegate(program_id: &Pubkey, params: &DepositNowParams) -> DelegateDerivation {
    delegate_pda(program_id, deposit_now_seed_hash(params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sra_common::{abi::u256_from_u64, constants::ACROSS_SPOKE_POOL_ID};

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
        let mut out = Vec::with_capacity(192 + params.message.len());
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

    #[test]
    fn deposit_now_seed_hash_matches_across_layout() {
        let params = sample_params();
        let expected = keccak_hash(&manual_seed_preimage(&params)).to_bytes();
        assert_eq!(deposit_now_seed_hash(&params), expected);
    }

    #[test]
    fn deposit_now_delegate_matches_pda_derivation() {
        let params = sample_params();
        let seed_hash = keccak_hash(&manual_seed_preimage(&params)).to_bytes();
        let (expected_pda, expected_bump) = Pubkey::find_program_address(
            &[DELEGATE_SEED_PREFIX, seed_hash.as_ref()],
            &ACROSS_SPOKE_POOL_ID,
        );

        let derived = deposit_now_delegate(&ACROSS_SPOKE_POOL_ID, &params);

        assert_eq!(derived.seed_hash, seed_hash);
        assert_eq!(derived.pda, expected_pda);
        assert_eq!(derived.bump, expected_bump);
    }

    #[test]
    fn deposit_now_seed_hash_changes_with_inputs() {
        let mut params = sample_params();
        let initial = deposit_now_seed_hash(&params);

        params.fill_deadline_offset += 1;
        assert_ne!(deposit_now_seed_hash(&params), initial);
    }
}
