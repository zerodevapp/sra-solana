//! Exact-layout deserialization of the CCTP v2 transmitter-owned accounts the
//! destination flow reads.
//!
//! Both accounts are pinned by PDA + owner in `verify_cctp_mint` and read as
//! raw bytes (never CPI'd), so this mirrors the `fill_status.rs` discipline:
//! validate the 8-byte Anchor discriminator, Borsh-decode the body, and
//! reject trailing bytes. Exactness is safe — Circle reallocs the transmitter
//! account to exact size on every attester enable/disable, and the layout was
//! verified against the live mainnet account (225 bytes, consumed exactly;
//! read 2026-07-28).

use anchor_lang::prelude::*;

use crate::CctpAdapterError;

/// `sha256("account:MessageTransmitter")[..8]` — hardcoded because the type
/// lives in Circle's crate; the pin test below recomputes it.
pub const MESSAGE_TRANSMITTER_DISCRIMINATOR: [u8; 8] =
    [0x47, 0x28, 0xb4, 0x8e, 0x13, 0xcb, 0x23, 0xfc];

/// `sha256("account:UsedNonce")[..8]`.
pub const USED_NONCE_DISCRIMINATOR: [u8; 8] = [0xd4, 0xde, 0x9d, 0xfc, 0x82, 0x47, 0xb3, 0xee];

/// Circle's MessageTransmitter v2 state (`["message_transmitter"]` PDA) —
/// field-for-field the upstream `MessageTransmitter` struct.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct MessageTransmitterState {
    pub owner: Pubkey,
    pub pending_owner: Pubkey,
    pub attester_manager: Pubkey,
    pub pauser: Pubkey,
    pub paused: bool,
    pub local_domain: u32,
    pub version: u32,
    pub signature_threshold: u32,
    pub enabled_attesters: Vec<Pubkey>,
    pub max_message_body_size: u64,
}

/// Circle's per-nonce mint receipt (`["used_nonce", nonce]` PDA), created by
/// `receive_message`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct UsedNonce {
    pub is_used: bool,
}

fn deserialize_exact<T: AnchorDeserialize>(
    data: &[u8],
    discriminator: &[u8; 8],
    err: CctpAdapterError,
) -> Result<T> {
    if data.len() < 8 || data[..8] != discriminator[..] {
        return Err(error!(err));
    }
    let mut body = &data[8..];
    let parsed = T::deserialize(&mut body).map_err(|_| error!(err))?;
    if !body.is_empty() {
        return Err(error!(err));
    }
    Ok(parsed)
}

/// Read the transmitter state out of raw account bytes and require it
/// internally consistent: a zero threshold would verify an empty attestation,
/// and a threshold above the attester count is unsatisfiable.
pub fn deserialize_message_transmitter(data: &[u8]) -> Result<MessageTransmitterState> {
    let state: MessageTransmitterState = deserialize_exact(
        data,
        &MESSAGE_TRANSMITTER_DISCRIMINATOR,
        CctpAdapterError::InvalidTransmitterState,
    )?;
    require!(
        state.signature_threshold > 0
            && state.signature_threshold as usize <= state.enabled_attesters.len(),
        CctpAdapterError::InvalidTransmitterState
    );
    Ok(state)
}

/// Read a used-nonce receipt out of raw account bytes. Malformed bytes error
/// as `NonceNotUsed` — either way the account fails to prove an executed mint.
pub fn deserialize_used_nonce(data: &[u8]) -> Result<UsedNonce> {
    deserialize_exact(
        data,
        &USED_NONCE_DISCRIMINATOR,
        CctpAdapterError::NonceNotUsed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program::hash as sha256;

    /// The discriminators are hardcoded (the account types live in Circle's
    /// crate) — recompute them so a typo cannot ship.
    #[test]
    fn discriminators_match_anchor_derivation() {
        assert_eq!(
            MESSAGE_TRANSMITTER_DISCRIMINATOR,
            sha256::hash(b"account:MessageTransmitter").to_bytes()[..8],
        );
        assert_eq!(
            USED_NONCE_DISCRIMINATOR,
            sha256::hash(b"account:UsedNonce").to_bytes()[..8],
        );
    }

    fn sample_state(signature_threshold: u32, attesters: usize) -> MessageTransmitterState {
        MessageTransmitterState {
            owner: Pubkey::new_from_array([1; 32]),
            pending_owner: Pubkey::default(),
            attester_manager: Pubkey::new_from_array([2; 32]),
            pauser: Pubkey::new_from_array([3; 32]),
            paused: false,
            local_domain: 5,
            version: 1,
            signature_threshold,
            enabled_attesters: (0..attesters)
                .map(|i| Pubkey::new_from_array([0x10 + i as u8; 32]))
                .collect(),
            max_message_body_size: 8192,
        }
    }

    fn state_bytes(state: &MessageTransmitterState) -> Vec<u8> {
        let mut data = MESSAGE_TRANSMITTER_DISCRIMINATOR.to_vec();
        state.serialize(&mut data).unwrap();
        data
    }

    fn used_nonce_bytes(is_used: bool) -> Vec<u8> {
        let mut data = USED_NONCE_DISCRIMINATOR.to_vec();
        UsedNonce { is_used }.serialize(&mut data).unwrap();
        data
    }

    #[test]
    fn transmitter_state_round_trips() {
        let state = sample_state(2, 3);
        let parsed = deserialize_message_transmitter(&state_bytes(&state)).unwrap();
        assert_eq!(parsed.local_domain, 5);
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.signature_threshold, 2);
        assert_eq!(parsed.enabled_attesters, state.enabled_attesters);
        assert_eq!(parsed.max_message_body_size, 8192);
    }

    #[test]
    fn transmitter_state_rejects_wrong_discriminator() {
        let mut bytes = state_bytes(&sample_state(1, 1));
        bytes[0] ^= 0xff;
        assert_eq!(
            deserialize_message_transmitter(&bytes).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState)
        );
    }

    #[test]
    fn transmitter_state_rejects_truncated_and_trailing_bytes() {
        let valid = state_bytes(&sample_state(1, 1));
        assert_eq!(
            deserialize_message_transmitter(&valid[..4]).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState),
            "shorter than the discriminator"
        );
        assert_eq!(
            deserialize_message_transmitter(&valid[..valid.len() - 1]).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState),
            "body truncated mid-layout"
        );
        let mut trailing = valid.clone();
        trailing.push(0);
        assert_eq!(
            deserialize_message_transmitter(&trailing).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState),
            "trailing byte past the exact layout"
        );
    }

    #[test]
    fn transmitter_state_rejects_zero_threshold() {
        assert_eq!(
            deserialize_message_transmitter(&state_bytes(&sample_state(0, 1))).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState)
        );
    }

    #[test]
    fn transmitter_state_rejects_threshold_above_attester_count() {
        assert_eq!(
            deserialize_message_transmitter(&state_bytes(&sample_state(2, 1))).unwrap_err(),
            error!(CctpAdapterError::InvalidTransmitterState)
        );
    }

    #[test]
    fn used_nonce_round_trips() {
        assert!(
            deserialize_used_nonce(&used_nonce_bytes(true))
                .unwrap()
                .is_used
        );
        assert!(
            !deserialize_used_nonce(&used_nonce_bytes(false))
                .unwrap()
                .is_used
        );
    }

    #[test]
    fn used_nonce_rejects_wrong_discriminator_truncated_and_trailing() {
        let valid = used_nonce_bytes(true);

        let mut wrong_disc = valid.clone();
        wrong_disc[0] ^= 0xff;
        assert_eq!(
            deserialize_used_nonce(&wrong_disc).unwrap_err(),
            error!(CctpAdapterError::NonceNotUsed)
        );
        assert_eq!(
            deserialize_used_nonce(&valid[..valid.len() - 1]).unwrap_err(),
            error!(CctpAdapterError::NonceNotUsed)
        );
        let mut trailing = valid.clone();
        trailing.push(0);
        assert_eq!(
            deserialize_used_nonce(&trailing).unwrap_err(),
            error!(CctpAdapterError::NonceNotUsed)
        );
    }
}
