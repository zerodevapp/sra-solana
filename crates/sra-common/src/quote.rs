//! Aggregate signed-quote scheme for verified-CPI actions. An authorized off-chain
//! quoter ed25519-signs a single root covering all VerifiedCpi actions of one fill;
//! the handler reconstructs the same root and verifies the signature once. Both the
//! quoter and the verifier use this module so they share one definition and cannot drift.

use anchor_lang::prelude::Pubkey;
use solana_keccak_hasher::hash as keccak_hash;

use crate::constants::SOLANA_CHAIN_ID;

/// Domain tag separating this v1 aggregate preimage from any other signed scheme.
pub const AGG_QUOTE_DOMAIN_TAG: [u8; 16] = *b"SRA_AGG_QUOTE_v1";

/// The ed25519-signed aggregate message is exactly the 32-byte keccak root.
pub const AGGREGATE_QUOTE_MESSAGE_LEN: usize = 32;

/// One VerifiedCpi action's individually-bound economic terms, in action order.
/// `input_amount` is the handler-derived `vault_amount_for_bps(snapshot, input_bps)`
/// (not a relayer free field); `min_out` is the signed floor for that action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotedAction {
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub input_amount: u64,
    pub min_out: u64,
}

/// The 32-byte ed25519 message: keccak256 over the domain-separated, ordered
/// commitment to every VerifiedCpi action of one fill. Shared by the off-chain
/// quoter and the on-chain verifier so they cannot drift. `actions` is in
/// action-config order; its length is bounded by MAX_ACTION_IDXS (fits u8).
pub fn aggregate_quote_root(
    route: &Pubkey,
    fill_id: &[u8; 32],
    expiry: i64,
    actions: &[QuotedAction],
) -> [u8; 32] {
    let mut data = Vec::with_capacity(16 + 32 + 32 + 8 + 8 + 1 + actions.len() * (32 + 32 + 8 + 8));
    data.extend_from_slice(&AGG_QUOTE_DOMAIN_TAG);
    data.extend_from_slice(route.as_ref());
    data.extend_from_slice(fill_id);
    data.extend_from_slice(&SOLANA_CHAIN_ID.to_le_bytes());
    data.extend_from_slice(&expiry.to_le_bytes());
    data.push(actions.len() as u8);
    for a in actions {
        data.extend_from_slice(a.input_mint.as_ref());
        data.extend_from_slice(a.output_mint.as_ref());
        data.extend_from_slice(&a.input_amount.to_le_bytes());
        data.extend_from_slice(&a.min_out.to_le_bytes());
    }
    keccak_hash(&data).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Pubkey {
        Pubkey::new_from_array([1u8; 32])
    }
    fn fill_id() -> [u8; 32] {
        [2u8; 32]
    }
    fn qa(i: u8, amt: u64, min: u64) -> QuotedAction {
        QuotedAction {
            input_mint: Pubkey::new_from_array([i; 32]),
            output_mint: Pubkey::new_from_array([i.wrapping_add(100); 32]),
            input_amount: amt,
            min_out: min,
        }
    }

    #[test]
    fn root_deterministic() {
        let a = [qa(1, 1000, 900), qa(2, 500, 480)];
        assert_eq!(
            aggregate_quote_root(&route(), &fill_id(), 1_700_000_000, &a),
            aggregate_quote_root(&route(), &fill_id(), 1_700_000_000, &a),
        );
    }

    #[test]
    fn root_binds_route_fill_expiry() {
        let a = [qa(1, 1000, 900)];
        let base = aggregate_quote_root(&route(), &fill_id(), 1, &a);
        assert_ne!(
            base,
            aggregate_quote_root(&Pubkey::new_from_array([9u8; 32]), &fill_id(), 1, &a)
        );
        assert_ne!(base, aggregate_quote_root(&route(), &[3u8; 32], 1, &a));
        assert_ne!(base, aggregate_quote_root(&route(), &fill_id(), 2, &a));
    }

    #[test]
    fn root_binds_each_min_out_and_input_amount() {
        let a = [qa(1, 1000, 900), qa(2, 500, 480)];
        let base = aggregate_quote_root(&route(), &fill_id(), 1, &a);
        let mut b = a;
        b[1].min_out = 481;
        assert_ne!(base, aggregate_quote_root(&route(), &fill_id(), 1, &b));
        let mut c = a;
        c[0].input_amount = 1001;
        assert_ne!(base, aggregate_quote_root(&route(), &fill_id(), 1, &c));
    }

    #[test]
    fn root_binds_order() {
        let a = [qa(1, 1000, 900), qa(2, 500, 480)];
        let swapped = [a[1], a[0]];
        assert_ne!(
            aggregate_quote_root(&route(), &fill_id(), 1, &a),
            aggregate_quote_root(&route(), &fill_id(), 1, &swapped),
        );
    }

    #[test]
    fn root_binds_count() {
        let two = [qa(1, 1000, 900), qa(2, 500, 480)];
        let one = [qa(1, 1000, 900)];
        assert_ne!(
            aggregate_quote_root(&route(), &fill_id(), 1, &two),
            aggregate_quote_root(&route(), &fill_id(), 1, &one),
        );
    }

    #[test]
    fn domain_tag_and_len_pinned() {
        assert_eq!(&AGG_QUOTE_DOMAIN_TAG, b"SRA_AGG_QUOTE_v1");
        assert_eq!(AGGREGATE_QUOTE_MESSAGE_LEN, 32);
    }
}
