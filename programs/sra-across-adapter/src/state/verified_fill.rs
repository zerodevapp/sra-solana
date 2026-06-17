use anchor_lang::prelude::*;

/// Adapter-owned verification artifact. Created by this adapter once it has
/// confirmed an Across fill, then read by the handler across the program boundary
/// to authorize `finalize_fill`. A vault-balance increase alone cannot prove a
/// legitimate bridge fill — this account is the trust boundary that does.
///
/// The post-discriminator field layout is a cross-program ABI contract with
/// `sra_common::VerifiedFillData`. The handler reads this account as raw bytes
/// (it must accept fills from multiple adapter programs, each with its own
/// program id, so it cannot import this type) and deserializes via that plain
/// struct, requiring an exact layout match. Any field reorder, addition, or
/// removal here is a breaking change and MUST be mirrored there — the round-trip
/// and discriminator tests below fail loudly if the two drift.
#[account]
#[derive(InitSpace)]
pub struct VerifiedFill {
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

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{AccountSerialize, Discriminator};
    use sra_common::types::{VerifiedFillData, VERIFIED_FILL_DISCRIMINATOR};

    fn sample() -> VerifiedFill {
        VerifiedFill {
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

    /// Serialize as Anchor would store it on-chain: 8-byte discriminator + Borsh body.
    fn serialize_account(vf: &VerifiedFill) -> Vec<u8> {
        let mut buf = Vec::new();
        vf.try_serialize(&mut buf).unwrap();
        buf
    }

    /// The cross-program ABI contract: bytes written by this adapter's `#[account]`
    /// must deserialize field-for-field through the handler's plain `VerifiedFillData`.
    #[test]
    fn verified_fill_serializes_and_handler_deserializes() {
        let vf = sample();
        let bytes = serialize_account(&vf);
        let data = VerifiedFillData::deserialize_from_account(&bytes).unwrap();
        assert_eq!(data.adapter, vf.adapter);
        assert_eq!(data.route, vf.route);
        assert_eq!(data.fill_id, vf.fill_id);
        assert_eq!(data.mint, vf.mint);
        assert_eq!(data.amount, vf.amount);
        assert_eq!(data.expected_amount, vf.expected_amount);
        assert_eq!(data.source_chain_id, vf.source_chain_id);
        assert_eq!(data.created_at, vf.created_at);
        assert_eq!(data.bump, vf.bump);
    }

    /// Drift canary: the handler hardcodes the discriminator (it reads VerifiedFill
    /// as raw bytes from many adapters, so it cannot import this type). If Anchor's
    /// derived discriminator ever diverges from that constant, the handler would
    /// reject every fill — catch it here instead.
    #[test]
    fn verified_fill_discriminator_matches_hardcoded() {
        assert_eq!(
            VerifiedFill::DISCRIMINATOR,
            VERIFIED_FILL_DISCRIMINATOR.as_slice()
        );
    }

    #[test]
    fn verified_fill_init_space_matches_layout() {
        // 32 adapter + 32 route + 32 fill_id + 32 mint + 8 amount
        // + 8 expected_amount + 8 source_chain_id + 8 created_at + 1 bump = 161
        assert_eq!(VerifiedFill::INIT_SPACE, 161);
    }

    /// The on-chain account is exactly the 8-byte discriminator plus the fixed-size
    /// body — no variable-length fields, so the serialized length is constant.
    #[test]
    fn verified_fill_account_size_matches_init_space() {
        let bytes = serialize_account(&sample());
        assert_eq!(bytes.len(), 8 + VerifiedFill::INIT_SPACE);
    }
}
