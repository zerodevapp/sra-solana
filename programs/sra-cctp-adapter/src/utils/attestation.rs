//! Attestation re-verification — an exact port of Circle's
//! `MessageTransmitter::verify_attestation_signatures`.
//!
//! The destination trust design re-runs the transmitter's own signature check
//! over `keccak256(message)` against the **live** attester set, binding the
//! caller-supplied message bytes to the same trust root the transmitter used
//! when it minted. Rules (Circle's, verbatim): the attestation is
//! `signature_threshold` concatenated 65-byte `r ‖ s ‖ v` signatures, each
//! with `v ∈ 27..=30` and low `s`, whose recovered EVM addresses are strictly
//! increasing (which also bans duplicates) and all enabled attesters.
//!
//! One deliberate deviation in mechanism, none in behavior: Circle rejects
//! high-`s` via libsecp's `is_high()`; on-chain we compare the 32 big-endian
//! `s` bytes against `n/2` directly, so the program carries no libsecp
//! dependency. (`s ≥ n` is also rejected: it exceeds `n/2` here, where Circle
//! fails signature parsing instead.)

use anchor_lang::prelude::*;
use solana_program::{keccak, secp256k1_recover::secp256k1_recover};

use crate::CctpAdapterError;

/// One concatenated attestation signature: `r(32) ‖ s(32) ‖ recovery_id(1)`.
pub const ATTESTATION_SIGNATURE_LEN: usize = 65;

/// The secp256k1 group order halved, big-endian. A signature with `s` above
/// this is the malleable twin of a canonical one and is rejected.
const SECP256K1_HALF_N: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// Verify `attestation` over `message_hash` against the transmitter's live
/// attester configuration. Every failure mode — length, recovery id, high-s,
/// unrecoverable signature, unknown/duplicate/misordered attester — surfaces
/// as `InvalidAttestation`.
pub fn verify_attestation_signatures(
    message_hash: &keccak::Hash,
    attestation: &[u8],
    signature_threshold: u32,
    enabled_attesters: &[Pubkey],
) -> Result<()> {
    require!(
        attestation.len() == ATTESTATION_SIGNATURE_LEN * signature_threshold as usize,
        CctpAdapterError::InvalidAttestation
    );

    let mut last_attester = Pubkey::default();
    for signature in attestation.chunks_exact(ATTESTATION_SIGNATURE_LEN) {
        let attester = recover_attester(message_hash, signature)?;
        // Strictly increasing recovered addresses: enforces Circle's canonical
        // ordering and bans duplicate signers in one comparison.
        require!(
            attester > last_attester,
            CctpAdapterError::InvalidAttestation
        );
        require!(
            enabled_attesters.contains(&attester),
            CctpAdapterError::InvalidAttestation
        );
        last_attester = attester;
    }
    Ok(())
}

/// Recover the signer's EVM address (last 20 bytes of `keccak(pubkey)`,
/// left-zero-padded to 32) — the wire format of `enabled_attesters`.
fn recover_attester(message_hash: &keccak::Hash, signature: &[u8]) -> Result<Pubkey> {
    let recovery_id = signature[ATTESTATION_SIGNATURE_LEN - 1];
    require!(
        (27..=30).contains(&recovery_id),
        CctpAdapterError::InvalidAttestation
    );
    require!(
        signature[32..64] <= SECP256K1_HALF_N[..],
        CctpAdapterError::InvalidAttestation
    );
    let pubkey = secp256k1_recover(message_hash.as_ref(), recovery_id - 27, &signature[..64])
        .map_err(|_| error!(CctpAdapterError::InvalidAttestation))?;

    let mut address = keccak::hash(&pubkey.to_bytes()).0;
    address[..12].fill(0);
    Ok(Pubkey::new_from_array(address))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsecp256k1::{sign, Message, PublicKey, SecretKey};

    fn key(byte: u8) -> SecretKey {
        SecretKey::parse(&[byte; 32]).expect("valid test secret key")
    }

    /// The attester address of a test key, exactly as `recover_attester`
    /// derives it (uncompressed pubkey without the 0x04 prefix).
    fn attester(sk: &SecretKey) -> Pubkey {
        let pubkey = PublicKey::from_secret_key(sk).serialize();
        let mut address = keccak::hash(&pubkey[1..]).0;
        address[..12].fill(0);
        Pubkey::new_from_array(address)
    }

    fn sign_one(hash: &keccak::Hash, sk: &SecretKey) -> Vec<u8> {
        let (signature, recovery_id) = sign(&Message::parse(&hash.0), sk);
        let mut out = signature.serialize().to_vec();
        out.push(recovery_id.serialize() + 27);
        out
    }

    /// Keys ordered so their recovered attester addresses strictly increase.
    fn ordered_keys(a: u8, b: u8) -> (SecretKey, SecretKey) {
        let (ka, kb) = (key(a), key(b));
        if attester(&ka) < attester(&kb) {
            (ka, kb)
        } else {
            (kb, ka)
        }
    }

    fn hash() -> keccak::Hash {
        keccak::hash(b"attested message bytes")
    }

    fn rejects(hash: &keccak::Hash, attestation: &[u8], threshold: u32, set: &[Pubkey], why: &str) {
        assert_eq!(
            verify_attestation_signatures(hash, attestation, threshold, set).unwrap_err(),
            error!(CctpAdapterError::InvalidAttestation),
            "{why}"
        );
    }

    #[test]
    fn one_of_one_accepts() {
        let sk = key(0x42);
        verify_attestation_signatures(&hash(), &sign_one(&hash(), &sk), 1, &[attester(&sk)])
            .expect("1-of-1 must verify");
    }

    #[test]
    fn two_of_two_accepts_in_ascending_order() {
        let (first, second) = ordered_keys(0x42, 0x43);
        let set = [attester(&first), attester(&second)];
        let mut attestation = sign_one(&hash(), &first);
        attestation.extend(sign_one(&hash(), &second));
        verify_attestation_signatures(&hash(), &attestation, 2, &set).expect("2-of-2 must verify");
    }

    #[test]
    fn wrong_signer_rejected() {
        let sk = key(0x42);
        rejects(
            &hash(),
            &sign_one(&hash(), &sk),
            1,
            &[attester(&key(0x43))],
            "a signer outside enabled_attesters must reject",
        );
    }

    #[test]
    fn duplicate_signer_rejected() {
        let sk = key(0x42);
        let set = [attester(&sk), attester(&key(0x43))];
        let mut attestation = sign_one(&hash(), &sk);
        attestation.extend(sign_one(&hash(), &sk));
        rejects(
            &hash(),
            &attestation,
            2,
            &set,
            "the same signer twice must reject (strictly increasing)",
        );
    }

    #[test]
    fn descending_order_rejected() {
        let (first, second) = ordered_keys(0x42, 0x43);
        let set = [attester(&first), attester(&second)];
        let mut attestation = sign_one(&hash(), &second);
        attestation.extend(sign_one(&hash(), &first));
        rejects(
            &hash(),
            &attestation,
            2,
            &set,
            "descending order must reject",
        );
    }

    #[test]
    fn high_s_rejected() {
        let sk = key(0x42);
        // Forge the malleable twin: s → n − s, recovery-id parity flipped. It
        // recovers the same signer, so only the high-s check can reject it.
        let (mut signature, recovery_id) = sign(&Message::parse(&hash().0), &sk);
        signature.s = -signature.s;
        let mut attestation = signature.serialize().to_vec();
        attestation.push((recovery_id.serialize() ^ 1) + 27);
        rejects(
            &hash(),
            &attestation,
            1,
            &[attester(&sk)],
            "high-s twin must reject",
        );
    }

    #[test]
    fn recovery_id_out_of_range_rejected() {
        let sk = key(0x42);
        for bad in [26, 31] {
            let mut attestation = sign_one(&hash(), &sk);
            attestation[64] = bad;
            rejects(
                &hash(),
                &attestation,
                1,
                &[attester(&sk)],
                "recovery id 26/31 must reject",
            );
        }
    }

    #[test]
    fn truncated_attestation_rejected() {
        let sk = key(0x42);
        let attestation = sign_one(&hash(), &sk);
        rejects(
            &hash(),
            &attestation[..64],
            1,
            &[attester(&sk)],
            "a truncated signature must reject",
        );
    }

    #[test]
    fn threshold_length_mismatch_rejected() {
        let (first, second) = ordered_keys(0x42, 0x43);
        let set = [attester(&first), attester(&second)];
        rejects(
            &hash(),
            &sign_one(&hash(), &first),
            2,
            &set,
            "one signature against threshold 2 must reject",
        );
    }

    #[test]
    fn signature_over_different_message_rejected() {
        let sk = key(0x42);
        let other_hash = keccak::hash(b"some other message");
        rejects(
            &hash(),
            &sign_one(&other_hash, &sk),
            1,
            &[attester(&sk)],
            "a signature over different bytes must reject",
        );
    }
}
