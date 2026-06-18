//! Exact-layout deserialization of the Across `FillStatusAccount`.
//!
//! The destination flow reads the Across fill-status PDA (`["fills", relay_hash]`)
//! as raw bytes — it is pinned by owner and PDA in the verify step, never CPI'd —
//! so this mirrors the handler's VerifiedFill discipline: validate the 8-byte
//! Anchor discriminator, Borsh-decode the body, and reject any trailing bytes so a
//! larger look-alike account cannot pass as a fill (Anchor's own `try_deserialize`
//! silently ignores trailing bytes).

use anchor_lang::{error, prelude::*, Discriminator};

use crate::errors::AcrossAdapterError;
use crate::utils::svm_spoke::{accounts::FillStatusAccount, types::FillStatus};

/// Length of the Anchor account discriminator prefix.
const DISCRIMINATOR_LEN: usize = 8;

/// Read the Across fill status out of raw `FillStatusAccount` bytes, enforcing the
/// exact on-chain layout (8-byte discriminator + Borsh body, no slack).
pub fn deserialize_fill_status(data: &[u8]) -> Result<FillStatus> {
    if data.len() < DISCRIMINATOR_LEN {
        return Err(error!(AcrossAdapterError::InvalidFillStatus));
    }
    // Reject any account whose discriminator is not Across's FillStatusAccount, so
    // an arbitrary look-alike cannot be read as a fill even if a caller ever omits
    // the owner/PDA pin (mirrors the handler's VerifiedFill discipline).
    if &data[..DISCRIMINATOR_LEN] != FillStatusAccount::DISCRIMINATOR {
        return Err(error!(AcrossAdapterError::InvalidFillStatus));
    }
    let mut body = &data[DISCRIMINATOR_LEN..];
    let account = FillStatusAccount::deserialize(&mut body)
        .map_err(|_| error!(AcrossAdapterError::InvalidFillStatus))?;
    if !body.is_empty() {
        return Err(error!(AcrossAdapterError::InvalidFillStatus));
    }
    Ok(account.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{AnchorSerialize, Discriminator};

    const RELAYER: Pubkey = Pubkey::new_from_array([9u8; 32]);
    const FILL_DEADLINE: u32 = 1_900_000_000;

    /// Build raw account bytes exactly as the chain stores them: the 8-byte
    /// discriminator followed by the Borsh body, with no slack.
    fn account_bytes(status: FillStatus) -> Vec<u8> {
        let account = FillStatusAccount {
            status,
            relayer: RELAYER,
            fill_deadline: FILL_DEADLINE,
        };
        let mut buf = FillStatusAccount::DISCRIMINATOR.to_vec();
        account.serialize(&mut buf).unwrap();
        buf
    }

    #[test]
    fn deserialize_fill_status_filled_round_trips() {
        let status = deserialize_fill_status(&account_bytes(FillStatus::Filled)).unwrap();
        assert!(matches!(status, FillStatus::Filled));
    }

    #[test]
    fn deserialize_fill_status_unfilled_round_trips() {
        let status = deserialize_fill_status(&account_bytes(FillStatus::Unfilled)).unwrap();
        assert!(matches!(status, FillStatus::Unfilled));
    }

    #[test]
    fn deserialize_fill_status_requested_slow_fill_round_trips() {
        // A slow-fill request is NOT a completed fill — the verify step must be
        // able to tell it apart, so pin its tag at the deserialization boundary.
        let status =
            deserialize_fill_status(&account_bytes(FillStatus::RequestedSlowFill)).unwrap();
        assert!(matches!(status, FillStatus::RequestedSlowFill));
    }

    #[test]
    fn deserialize_fill_status_rejects_wrong_discriminator() {
        let mut bytes = account_bytes(FillStatus::Filled);
        bytes[0] ^= 0xff;
        assert_eq!(
            deserialize_fill_status(&bytes).unwrap_err(),
            error!(AcrossAdapterError::InvalidFillStatus)
        );
    }

    #[test]
    fn deserialize_fill_status_rejects_truncated_and_trailing_bytes() {
        let valid = account_bytes(FillStatus::Filled);
        // 8 discriminator + 1 status + 32 relayer + 4 fill_deadline.
        assert_eq!(valid.len(), 45);

        // Shorter than the discriminator.
        assert_eq!(
            deserialize_fill_status(&valid[..4]).unwrap_err(),
            error!(AcrossAdapterError::InvalidFillStatus)
        );
        // Body truncated mid-layout.
        assert_eq!(
            deserialize_fill_status(&valid[..valid.len() - 1]).unwrap_err(),
            error!(AcrossAdapterError::InvalidFillStatus)
        );
        // Trailing byte past the exact layout.
        let mut trailing = valid.clone();
        trailing.push(0);
        assert_eq!(
            deserialize_fill_status(&trailing).unwrap_err(),
            error!(AcrossAdapterError::InvalidFillStatus)
        );
    }
}
