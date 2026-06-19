//! Signed price-quote scheme for verified-CPI `min_out` resolution.
//!
//! A cross-asset action (e.g. swap USDC -> SOL) has no meaningful slippage bound
//! without a reference price. An authorized off-chain quoter signs a per-action
//! quote naming the minimum acceptable output; the handler verifies that signature
//! (ed25519) and uses the signed `min_amount_out` as the balance-diff floor.
//!
//! The quote is bound to a per-fill, per-action **context hash** so a quote signed
//! for one action of one fill cannot be replayed against another action or another
//! fill. This mirrors the reference resolver's context binding: the signer commits
//! to `(context, input, output, input_amount, chain, min_out, expiry)` and the
//! verifier reconstructs the identical message before matching the signature.
//!
//! Both functions are pure (no account reads) so the off-chain quoter and the
//! on-chain verifier share one definition and cannot drift.

use anchor_lang::prelude::Pubkey;
use solana_keccak_hasher::hash as keccak_hash;

use crate::constants::SOLANA_CHAIN_ID;

/// Exact byte length of a signed-quote message. Pinned so the on-chain verifier
/// and the off-chain quoter agree on the layout.
pub const SIGNED_QUOTE_MESSAGE_LEN: usize = 32 + 32 + 32 + 8 + 8 + 8 + 8;

/// Per-fill, per-action binding hash: `keccak256(route ‖ fill_id ‖ action_index)`.
///
/// Folding `action_index` in means a quote signed for action *i* of a fill cannot
/// satisfy action *j*; folding `route` and `fill_id` in means it cannot be
/// replayed against a different route or a different fill of the same route.
pub fn action_context_hash(route: &Pubkey, fill_id: &[u8; 32], action_index: u8) -> [u8; 32] {
    let mut data = [0u8; 32 + 32 + 1];
    data[..32].copy_from_slice(route.as_ref());
    data[32..64].copy_from_slice(fill_id);
    data[64] = action_index;
    keccak_hash(&data).to_bytes()
}

/// The exact bytes an authorized quoter signs and the handler reconstructs.
///
/// Layout (little-endian scalars):
/// `context_hash(32) ‖ input_mint(32) ‖ output_mint(32) ‖ input_amount(8) ‖`
/// `min_amount_out(8) ‖ expiry(8) ‖ SOLANA_CHAIN_ID(8)`.
///
/// The chain id is a domain separator: a quote signed for Solana cannot be
/// replayed on another chain that adopts the same scheme.
pub fn signed_quote_message(
    context_hash: &[u8; 32],
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    input_amount: u64,
    min_amount_out: u64,
    expiry: i64,
) -> [u8; SIGNED_QUOTE_MESSAGE_LEN] {
    let mut msg = [0u8; SIGNED_QUOTE_MESSAGE_LEN];
    msg[..32].copy_from_slice(context_hash);
    msg[32..64].copy_from_slice(input_mint.as_ref());
    msg[64..96].copy_from_slice(output_mint.as_ref());
    msg[96..104].copy_from_slice(&input_amount.to_le_bytes());
    msg[104..112].copy_from_slice(&min_amount_out.to_le_bytes());
    msg[112..120].copy_from_slice(&expiry.to_le_bytes());
    msg[120..128].copy_from_slice(&SOLANA_CHAIN_ID.to_le_bytes());
    msg
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

    #[test]
    fn action_context_hash_deterministic() {
        let h1 = action_context_hash(&route(), &fill_id(), 0);
        let h2 = action_context_hash(&route(), &fill_id(), 0);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn action_context_hash_binds_action_index() {
        assert_ne!(
            action_context_hash(&route(), &fill_id(), 0),
            action_context_hash(&route(), &fill_id(), 1),
        );
    }

    #[test]
    fn action_context_hash_binds_route() {
        let other = Pubkey::new_from_array([9u8; 32]);
        assert_ne!(
            action_context_hash(&route(), &fill_id(), 0),
            action_context_hash(&other, &fill_id(), 0),
        );
    }

    #[test]
    fn action_context_hash_binds_fill_id() {
        assert_ne!(
            action_context_hash(&route(), &fill_id(), 0),
            action_context_hash(&route(), &[3u8; 32], 0),
        );
    }

    #[test]
    fn signed_quote_message_length_is_pinned() {
        let ctx = action_context_hash(&route(), &fill_id(), 0);
        let msg = signed_quote_message(
            &ctx,
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            1_000,
            900,
            1_700_000_000,
        );
        assert_eq!(msg.len(), SIGNED_QUOTE_MESSAGE_LEN);
        assert_eq!(SIGNED_QUOTE_MESSAGE_LEN, 128);
    }

    #[test]
    fn signed_quote_message_layout_is_exact() {
        // The off-chain quoter must reproduce these bytes exactly — pin the layout
        // field by field (no keccak needed; the context hash is opaque here).
        let ctx = [7u8; 32];
        let input_mint = Pubkey::new_from_array([11u8; 32]);
        let output_mint = Pubkey::new_from_array([22u8; 32]);
        let input_amount: u64 = 0x0102_0304_0506_0708;
        let min_amount_out: u64 = 0x1112_1314_1516_1718;
        let expiry: i64 = 0x2122_2324_2526_2728;
        let msg = signed_quote_message(
            &ctx,
            &input_mint,
            &output_mint,
            input_amount,
            min_amount_out,
            expiry,
        );

        assert_eq!(&msg[..32], &ctx);
        assert_eq!(&msg[32..64], input_mint.as_ref());
        assert_eq!(&msg[64..96], output_mint.as_ref());
        assert_eq!(&msg[96..104], &input_amount.to_le_bytes());
        assert_eq!(&msg[104..112], &min_amount_out.to_le_bytes());
        assert_eq!(&msg[112..120], &expiry.to_le_bytes());
        assert_eq!(&msg[120..128], &SOLANA_CHAIN_ID.to_le_bytes());
    }

    #[test]
    fn signed_quote_message_binds_min_amount_out() {
        let ctx = [7u8; 32];
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        assert_ne!(
            signed_quote_message(&ctx, &a, &b, 1_000, 900, 1).to_vec(),
            signed_quote_message(&ctx, &a, &b, 1_000, 901, 1).to_vec(),
        );
    }

    #[test]
    fn signed_quote_message_binds_token_pair() {
        let ctx = [7u8; 32];
        let a = Pubkey::new_from_array([1u8; 32]);
        let b = Pubkey::new_from_array([2u8; 32]);
        // Swapping which token is input vs output changes the signed bytes.
        assert_ne!(
            signed_quote_message(&ctx, &a, &b, 1_000, 900, 1).to_vec(),
            signed_quote_message(&ctx, &b, &a, 1_000, 900, 1).to_vec(),
        );
    }
}
