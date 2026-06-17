use anchor_lang::prelude::*;

/// Plain struct for handler to manually deserialize adapter-owned VerifiedFill.
///
/// NOT #[account] — this has no Owner impl. The actual VerifiedFill #[account]
/// type lives in the adapter crate with the adapter's declare_id!.
///
/// The handler reads VerifiedFill as UncheckedAccount, validates that
/// account.owner is in route.allowed_adapters, then deserializes using this struct.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct VerifiedFillData {
    pub adapter: Pubkey,
    pub route: Pubkey,
    pub fill_id: [u8; 32],
    pub mint: Pubkey,
    pub amount: u64,
    pub expected_amount: u64,
    pub source_chain_id: u64,
    pub created_at: i64,
    pub bump: u8,
}

/// Anchor discriminator for the VerifiedFill #[account] type.
/// = sha256("account:VerifiedFill")[..8]
/// Shared between adapter (which creates it) and handler (which validates it).
///
/// This must be manually defined because VerifiedFillData is a plain struct (not
/// #[account]) — the actual #[account] VerifiedFill lives in the adapter crate,
/// tied to the adapter's program ID via Anchor's Owner impl. The handler can't
/// import that type directly since it needs to accept VerifiedFill accounts from
/// *multiple* allowed adapters, each with a different program ID. Instead it reads
/// raw UncheckedAccount bytes, checks owner against route.allowed_adapters, and
/// deserializes manually. We hardcode the discriminator here so we can still
/// reject type-confusion attacks (arbitrary adapter-owned accounts that happen to
/// Borsh-deserialize into the right shape).
pub const VERIFIED_FILL_DISCRIMINATOR: [u8; 8] = [0x02, 0xe4, 0x43, 0x60, 0xfe, 0x4a, 0x6a, 0x3a];

impl VerifiedFillData {
    /// Deserialize from raw account data, validating the 8-byte Anchor discriminator.
    ///
    /// Without discriminator validation, any account owned by an allowed adapter
    /// that happens to Borsh-deserialize into the right shape could be misread
    /// as a VerifiedFill (type confusion attack).
    pub fn deserialize_from_account(data: &[u8]) -> Result<Self> {
        if data.len() < 8 {
            return Err(error!(crate::errors::SraError::InvalidFill));
        }
        if data[..8] != VERIFIED_FILL_DISCRIMINATOR {
            return Err(error!(crate::errors::SraError::InvalidFill));
        }
        let mut slice = &data[8..];
        let result = Self::deserialize(&mut slice)
            .map_err(|_| error!(crate::errors::SraError::InvalidFill))?;
        // Reject accounts with trailing bytes — exact layout match required.
        // Without this, a larger adapter-owned account with a matching prefix
        // would pass, masking handler/adapter schema drift.
        if !slice.is_empty() {
            return Err(error!(crate::errors::SraError::InvalidFill));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::SraError;

    fn sample() -> VerifiedFillData {
        VerifiedFillData {
            adapter: Pubkey::new_from_array([1u8; 32]),
            route: Pubkey::new_from_array([2u8; 32]),
            fill_id: [3u8; 32],
            mint: Pubkey::new_from_array([4u8; 32]),
            amount: 1_000_000,
            expected_amount: 999_999,
            source_chain_id: 42,
            created_at: 1_700_000_000,
            bump: 254,
        }
    }

    /// Exactly the on-chain layout: 8-byte discriminator + Borsh body, no slack.
    fn valid_account_bytes() -> Vec<u8> {
        let mut buf = VERIFIED_FILL_DISCRIMINATOR.to_vec();
        sample().serialize(&mut buf).unwrap();
        buf
    }

    #[test]
    fn deserialize_from_account_accepts_exact_layout() {
        let decoded = VerifiedFillData::deserialize_from_account(&valid_account_bytes()).unwrap();
        assert_eq!(decoded.fill_id, sample().fill_id);
        assert_eq!(decoded.amount, sample().amount);
        assert_eq!(decoded.bump, sample().bump);
    }

    #[test]
    fn deserialize_from_account_rejects_too_short() {
        let err = VerifiedFillData::deserialize_from_account(&[0u8; 4])
            .expect_err("data shorter than the discriminator must reject");
        assert_eq!(err, error!(SraError::InvalidFill));
    }

    #[test]
    fn deserialize_from_account_rejects_wrong_discriminator() {
        let mut bytes = valid_account_bytes();
        bytes[0] ^= 0xff;
        let err = VerifiedFillData::deserialize_from_account(&bytes)
            .expect_err("a non-VerifiedFill discriminator must reject (type-confusion defense)");
        assert_eq!(err, error!(SraError::InvalidFill));
    }

    #[test]
    fn deserialize_from_account_rejects_truncated_body() {
        // Valid discriminator, but the Borsh body is cut short of the layout.
        let bytes = valid_account_bytes();
        let err = VerifiedFillData::deserialize_from_account(&bytes[..bytes.len() - 1])
            .expect_err("a body shorter than the layout must reject");
        assert_eq!(err, error!(SraError::InvalidFill));
    }

    #[test]
    fn deserialize_from_account_rejects_trailing_bytes() {
        // A larger adapter-owned account whose prefix happens to match must not pass —
        // trailing bytes surface handler/adapter schema drift as a hard failure.
        let mut bytes = valid_account_bytes();
        bytes.push(0);
        let err = VerifiedFillData::deserialize_from_account(&bytes)
            .expect_err("trailing bytes past the exact layout must reject");
        assert_eq!(err, error!(SraError::InvalidFill));
    }
}
