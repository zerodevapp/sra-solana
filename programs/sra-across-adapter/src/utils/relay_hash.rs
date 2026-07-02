//! Across relay-hash computation and message hashing.
//!
//! Byte-exact port of the deployed Across SVM Spoke relay-hash derivation. The
//! fill-status account the destination flow verifies lives at the PDA
//! `["fills", relay_hash]`, so any divergence here makes the adapter look up the
//! wrong account and every genuine fill fails to verify — the same drift class
//! that stranded a deposit when the vendored deposit IDL fell out of sync with the
//! deployed program. The authoritative cross-check against a real on-chain fill
//! lands in the destination fork end-to-end suite; the unit tests below pin the
//! preimage layout independently and freeze the result against accidental drift.

use anchor_lang::{error, prelude::*};
use solana_program::keccak::hash as keccak_hash;

use crate::errors::AcrossAdapterError;
use crate::utils::svm_spoke::types::RelayData;

/// Width of the Borsh length prefix Across emits before the `message` bytes.
const BORSH_LEN_PREFIX: usize = 4;

/// Hash of the relay message, matching Across exactly: an empty message hashes to
/// all-zero bytes (NOT `keccak256("")`) so the relay hash can be reconstructed
/// from the emitted fill event, which carries this hash rather than the raw
/// message.
pub fn hash_non_empty_message(message: &[u8]) -> [u8; 32] {
    if message.is_empty() {
        [0u8; 32]
    } else {
        keccak_hash(message).to_bytes()
    }
}

/// `keccak256(borsh(relay_data, message replaced by its hash) || chain_id_le)` —
/// the deployed Across SVM Spoke relay hash. `chain_id` is the destination chain
/// id Across binds into the fill (this chain, for fills landing here).
///
/// Mirrors Across's own trick: Borsh-serialize the whole struct, then trim the
/// trailing message field (4-byte length prefix + bytes) and append its hash. This
/// is byte-identical to serializing every field except `message` and is cheaper
/// than re-serializing field by field.
pub fn compute_relay_hash(relay_data: &RelayData, chain_id: u64) -> Result<[u8; 32]> {
    let mut input = relay_data
        .try_to_vec()
        .map_err(|_| error!(AcrossAdapterError::RelayHashSerializationFailed))?;
    // Fail closed rather than underflow (debug panic) or wrap into a no-op
    // truncate + wrong hash (release) if the serialized layout ever stops ending
    // with the `message` field — on-chain code must never silently mis-hash.
    let prefix_len = input
        .len()
        .checked_sub(BORSH_LEN_PREFIX + relay_data.message.len())
        .ok_or_else(|| error!(AcrossAdapterError::RelayHashSerializationFailed))?;
    input.truncate(prefix_len);
    input.extend_from_slice(&hash_non_empty_message(&relay_data.message));
    input.extend_from_slice(&chain_id.to_le_bytes());
    Ok(keccak_hash(&input).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The destination chain id Across binds for fills landing on Solana. The hash
    // is parameterized, so the tests also vary it independently.
    const CHAIN_ID: u64 = sra_common::constants::SOLANA_CHAIN_ID;

    fn sample() -> RelayData {
        RelayData {
            depositor: Pubkey::new_from_array([1u8; 32]),
            recipient: Pubkey::new_from_array([2u8; 32]),
            exclusive_relayer: Pubkey::new_from_array([3u8; 32]),
            input_token: Pubkey::new_from_array([4u8; 32]),
            output_token: Pubkey::new_from_array([5u8; 32]),
            input_amount: [6u8; 32],
            output_amount: 9_998_500,
            origin_chain_id: 8_453,
            deposit_id: [7u8; 32],
            fill_deadline: 1_900_000_000,
            exclusivity_deadline: 1_899_900_000,
            message: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    /// Independent reconstruction of the Across preimage, field by field in the
    /// documented order, so the struct-truncate trick in `compute_relay_hash` is
    /// cross-checked against a hand-built byte layout (the seed-hash precedent).
    fn manual_relay_hash(rd: &RelayData, chain_id: u64) -> [u8; 32] {
        let mut p = Vec::new();
        p.extend_from_slice(rd.depositor.as_ref());
        p.extend_from_slice(rd.recipient.as_ref());
        p.extend_from_slice(rd.exclusive_relayer.as_ref());
        p.extend_from_slice(rd.input_token.as_ref());
        p.extend_from_slice(rd.output_token.as_ref());
        p.extend_from_slice(&rd.input_amount);
        p.extend_from_slice(&rd.output_amount.to_le_bytes());
        p.extend_from_slice(&rd.origin_chain_id.to_le_bytes());
        p.extend_from_slice(&rd.deposit_id);
        p.extend_from_slice(&rd.fill_deadline.to_le_bytes());
        p.extend_from_slice(&rd.exclusivity_deadline.to_le_bytes());
        p.extend_from_slice(&hash_non_empty_message(&rd.message));
        p.extend_from_slice(&chain_id.to_le_bytes());
        keccak_hash(&p).to_bytes()
    }

    fn encode_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    #[test]
    fn compute_relay_hash_deterministic() {
        assert_eq!(
            compute_relay_hash(&sample(), CHAIN_ID).unwrap(),
            compute_relay_hash(&sample(), CHAIN_ID).unwrap()
        );
    }

    #[test]
    fn compute_relay_hash_matches_across_layout() {
        // Empty, short, and large messages all exercise the truncate-and-replace path.
        for message in [vec![], vec![0x11, 0x22, 0x33, 0x44], vec![0x5au8; 512]] {
            let mut rd = sample();
            rd.message = message;
            assert_eq!(
                compute_relay_hash(&rd, CHAIN_ID).unwrap(),
                manual_relay_hash(&rd, CHAIN_ID),
            );
        }
    }

    #[test]
    fn compute_relay_hash_regression_pin() {
        // Frozen value for the canonical sample. The authoritative cross-check
        // is the destination fork suite, where the real Across program accepts
        // (and previously rejected) this preimage; this pin catches accidental
        // drift between fork runs.
        assert_eq!(
            encode_hex(&compute_relay_hash(&sample(), CHAIN_ID).unwrap()),
            "f4a171530857981a67dba8ebbd810db29fb7c222ad37f0dbbd39479788f8fb67"
        );
    }

    fn assert_field_changes_hash(mutate: impl FnOnce(&mut RelayData)) {
        let base = compute_relay_hash(&sample(), CHAIN_ID).unwrap();
        let mut rd = sample();
        mutate(&mut rd);
        assert_ne!(
            compute_relay_hash(&rd, CHAIN_ID).unwrap(),
            base,
            "mutating a relay-data field must change the relay hash"
        );
    }

    #[test]
    fn compute_relay_hash_changes_with_each_field() {
        assert_field_changes_hash(|rd| rd.depositor = Pubkey::new_from_array([0x99; 32]));
        assert_field_changes_hash(|rd| rd.recipient = Pubkey::new_from_array([0x99; 32]));
        assert_field_changes_hash(|rd| rd.exclusive_relayer = Pubkey::new_from_array([0x99; 32]));
        assert_field_changes_hash(|rd| rd.input_token = Pubkey::new_from_array([0x99; 32]));
        assert_field_changes_hash(|rd| rd.output_token = Pubkey::new_from_array([0x99; 32]));
        assert_field_changes_hash(|rd| rd.input_amount = [0x99; 32]);
        assert_field_changes_hash(|rd| rd.output_amount += 1);
        assert_field_changes_hash(|rd| rd.origin_chain_id += 1);
        assert_field_changes_hash(|rd| rd.deposit_id = [0x99; 32]);
        assert_field_changes_hash(|rd| rd.fill_deadline += 1);
        assert_field_changes_hash(|rd| rd.exclusivity_deadline += 1);
        assert_field_changes_hash(|rd| rd.message = vec![0x00]);
    }

    #[test]
    fn compute_relay_hash_changes_with_chain_id() {
        let base = compute_relay_hash(&sample(), CHAIN_ID).unwrap();
        assert_ne!(compute_relay_hash(&sample(), CHAIN_ID + 1).unwrap(), base);
    }

    #[test]
    fn hash_non_empty_message_empty_returns_zero_bytes32() {
        assert_eq!(hash_non_empty_message(&[]), [0u8; 32]);
        // Specifically NOT keccak256 of an empty input.
        assert_ne!(hash_non_empty_message(&[]), keccak_hash(&[]).to_bytes());
    }

    #[test]
    fn hash_non_empty_message_non_empty_returns_keccak256() {
        let message = b"across-relay-message";
        assert_eq!(
            hash_non_empty_message(message),
            keccak_hash(message).to_bytes()
        );
        assert_ne!(hash_non_empty_message(message), [0u8; 32]);
    }
}
