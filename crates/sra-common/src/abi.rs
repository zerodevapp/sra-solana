//! Wire-encoding helpers for the Across bridge boundary.
//!
//! The two-transaction target-execution model bridges an empty Across
//! envelope, so the cross-chain SRA `Message` ABI and its replay encoders are
//! gone. What remains is the single widener used to render a `u64` as the
//! big-endian `uint256` that Across's `deposit_now` `output_amount` field
//! expects (and that `source_deposit::decode_output_amount` reads back).

/// Size of a big-endian `uint256` word in bytes.
const WORD: usize = 32;

/// Encode a `u64` as a big-endian `uint256` word.
///
/// Chain IDs, nonces, and amounts are stored as `u64` across the codebase; this
/// widens them into the `[u8; 32]` representation the Across `output_amount`
/// wire field uses — value in the rightmost 8 bytes, upper 24 zero-filled.
pub fn u256_from_u64(v: u64) -> [u8; WORD] {
    let mut r = [0u8; WORD];
    r[WORD - 8..].copy_from_slice(&v.to_be_bytes());
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_from_u64_is_big_endian_low_eight_bytes() {
        let v = u256_from_u64(0x0102_0304_0506_0708);
        assert_eq!(&v[..24], &[0u8; 24]);
        assert_eq!(&v[24..], &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    }

    #[test]
    fn u256_from_u64_zero_and_max() {
        assert_eq!(u256_from_u64(0), [0u8; 32]);
        let mut max = [0u8; 32];
        max[24..].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(u256_from_u64(u64::MAX), max);
    }
}
