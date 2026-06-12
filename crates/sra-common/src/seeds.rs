use anchor_lang::prelude::*;
use solana_keccak_hasher::hash as keccak_hash;

use crate::types::{SolanaAction, SourceHandlerData};

// ---------------------------------------------------------------------------
// PDA seed prefixes
// ---------------------------------------------------------------------------

// Source side (handler-owned)
pub const SEED_SRA: &[u8] = b"sra";
pub const SEED_MINT_NONCE: &[u8] = b"mint_nonce";
pub const SEED_ADDRESS_BOOK: &[u8] = b"address_book";

// Destination side (handler-owned)
pub const SEED_SRA_ROUTE: &[u8] = b"sra_route";
pub const SEED_FILL_RECEIPT: &[u8] = b"fill_receipt";
pub const SEED_EXECUTION: &[u8] = b"execution";

// Shared (handler-owned)
pub const SEED_FEE_CONFIG: &[u8] = b"fee_config";
pub const SEED_PROTOCOL_FEE: &[u8] = b"protocol_fee";
pub const SEED_ASSET_FEE_TIER: &[u8] = b"asset_fee_tier";

// Adapter-owned
pub const SEED_ADAPTER_AUTHORITY: &[u8] = b"adapter_authority";
pub const SEED_VERIFIED_FILL: &[u8] = b"verified_fill";

/// Signer seeds for the `adapter_authority` PDA that owns adapter custody —
/// the single source for the seed scheme shared by the deposit and recovery
/// instructions. Pass as `&[&seeds]` to `CpiContext::new_with_signer`.
pub fn adapter_authority_signer_seeds<'a>(
    sra_config_key: &'a [u8],
    bump: &'a [u8; 1],
) -> [&'a [u8]; 3] {
    [SEED_ADAPTER_AUTHORITY, sra_config_key, bump]
}

// ---------------------------------------------------------------------------
// Source config_hash
// ---------------------------------------------------------------------------

/// Pure hash. Callers must `validate()` separately — bundling validation here
/// would panic during PDA seed derivation.
pub fn handler_data_hash(handler_data: &SourceHandlerData) -> [u8; 32] {
    let serialized = handler_data
        .try_to_vec()
        .expect("SourceHandlerData serialization should not fail");
    keccak_hash(&serialized).to_bytes()
}

/// PDA seeds: ["sra", owner, config_hash, salt]
/// config_hash = keccak256(owner ‖ final_handler ‖ handler_data_hash)
pub fn config_hash(
    owner: &Pubkey,
    final_handler: &Pubkey,
    handler_data_hash: &[u8; 32],
) -> [u8; 32] {
    let mut data = Vec::with_capacity(32 + 32 + 32);
    data.extend_from_slice(owner.as_ref());
    data.extend_from_slice(final_handler.as_ref());
    data.extend_from_slice(handler_data_hash);
    keccak_hash(&data).to_bytes()
}

// ---------------------------------------------------------------------------
// Destination route_config_hash
// ---------------------------------------------------------------------------

/// Compute the config_hash for a destination route PDA.
/// PDA seeds: ["sra_route", owner, config_hash, salt]
/// config_hash = keccak256(borsh(owner, source_chain_id, expected_mint, app_id, action_config))
pub fn route_config_hash(
    owner: &Pubkey,
    source_chain_id: u64,
    expected_mint: &Pubkey,
    app_id: &[u8; 32],
    action_config: &[SolanaAction],
) -> Result<[u8; 32]> {
    require!(
        action_config.len() <= crate::constants::MAX_ACTION_IDXS,
        crate::errors::SraError::TooManyActionIdxs,
    );
    let mut data = Vec::with_capacity(32 + 8 + 32 + 32 + 4 + action_config.len() * 35);
    data.extend_from_slice(owner.as_ref());
    data.extend_from_slice(&source_chain_id.to_le_bytes());
    data.extend_from_slice(expected_mint.as_ref());
    data.extend_from_slice(app_id);
    // Borsh-serialize the action_config Vec
    let serialized = action_config
        .try_to_vec()
        .expect("SolanaAction serialization should not fail");
    data.extend_from_slice(&serialized);
    Ok(keccak_hash(&data).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MAX_ADAPTERS, MAX_SLIPPAGE_BPS};

    fn test_owner() -> Pubkey {
        Pubkey::new_from_array([1u8; 32])
    }

    fn test_final_handler() -> Pubkey {
        Pubkey::new_from_array([2u8; 32])
    }

    fn test_handler_data() -> SourceHandlerData {
        SourceHandlerData {
            address_book_root: Pubkey::new_from_array([3u8; 32]),
            target_chain_id: 42_161,
            target_handler: [4u8; 32],
            target_adapter: [5u8; 32],
            recipient: [6u8; 32],
            app_id: [7u8; 32],
            target_token: [8u8; 32],
            allowed_slippage_bps: 50,
            allowed_adapters: vec![Pubkey::new_from_array([9u8; 32])],
        }
    }

    fn test_app_id() -> [u8; 32] {
        [5u8; 32]
    }

    #[test]
    fn handler_data_hash_deterministic() {
        let handler_data = test_handler_data();
        let h1 = handler_data_hash(&handler_data);
        let h2 = handler_data_hash(&handler_data);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn handler_data_hash_different_payload() {
        let handler_data = test_handler_data();
        let mut different = handler_data.clone();
        different.recipient = [9u8; 32];
        assert_ne!(
            handler_data_hash(&handler_data),
            handler_data_hash(&different)
        );
    }

    #[test]
    fn handler_data_hash_does_not_validate() {
        // Regression guard: re-bundling validate() would panic during seeds.
        let mut handler_data = test_handler_data();
        handler_data.allowed_slippage_bps = MAX_SLIPPAGE_BPS + 1;
        let _ = handler_data_hash(&handler_data);
    }

    #[test]
    fn validate_rejects_too_many_adapters() {
        let mut handler_data = test_handler_data();
        handler_data.allowed_adapters = (0..=MAX_ADAPTERS)
            .map(|i| Pubkey::new_from_array([i as u8; 32]))
            .collect();

        let err = handler_data.validate().unwrap_err();
        match err {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, "TooManyAdapters");
            }
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_invalid_slippage() {
        let mut handler_data = test_handler_data();
        handler_data.allowed_slippage_bps = MAX_SLIPPAGE_BPS + 1;

        let err = handler_data.validate().unwrap_err();
        match err {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, "InvalidSlippage");
            }
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    #[test]
    fn config_hash_deterministic() {
        let data_hash = handler_data_hash(&test_handler_data());
        let h1 = config_hash(&test_owner(), &test_final_handler(), &data_hash);
        let h2 = config_hash(&test_owner(), &test_final_handler(), &data_hash);
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn config_hash_different_final_handler() {
        let data_hash = handler_data_hash(&test_handler_data());
        let h1 = config_hash(&test_owner(), &test_final_handler(), &data_hash);
        let h2 = config_hash(
            &test_owner(),
            &Pubkey::new_from_array([7u8; 32]),
            &data_hash,
        );
        assert_ne!(h1, h2);
    }

    #[test]
    fn config_hash_different_owner() {
        let owner2 = Pubkey::new_from_array([99u8; 32]);
        let data_hash = handler_data_hash(&test_handler_data());
        let h1 = config_hash(&test_owner(), &test_final_handler(), &data_hash);
        let h2 = config_hash(&owner2, &test_final_handler(), &data_hash);
        assert_ne!(h1, h2);
    }

    #[test]
    fn config_hash_different_handler_data() {
        let data_hash = handler_data_hash(&test_handler_data());
        let mut different_handler_data = test_handler_data();
        different_handler_data.target_adapter = [11u8; 32];
        let different_handler_data_hash = handler_data_hash(&different_handler_data);

        let h1 = config_hash(&test_owner(), &test_final_handler(), &data_hash);
        let h2 = config_hash(
            &test_owner(),
            &test_final_handler(),
            &different_handler_data_hash,
        );
        assert_ne!(h1, h2);
    }

    #[test]
    fn route_config_hash_deterministic() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions = vec![SolanaAction::SplTransfer {
            recipient: Pubkey::new_from_array([20u8; 32]),
            amount_bps: 10000,
        }];
        let h1 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap();
        let h2 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap();
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn route_config_hash_different_chain() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions = vec![];
        let h1 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap();
        let h2 = route_config_hash(&test_owner(), 10, &mint, &test_app_id(), &actions).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn route_config_hash_different_actions() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions1 = vec![SolanaAction::SplTransfer {
            recipient: Pubkey::new_from_array([20u8; 32]),
            amount_bps: 5000,
        }];
        let actions2 = vec![SolanaAction::SplTransfer {
            recipient: Pubkey::new_from_array([20u8; 32]),
            amount_bps: 10000,
        }];
        let h1 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions1).unwrap();
        let h2 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions2).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn route_config_hash_empty_actions() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let h1 = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &[]).unwrap();
        assert_ne!(h1, [0u8; 32]);
    }

    #[test]
    fn route_config_hash_does_not_validate() {
        // Guard: route_config_hash feeds `seeds = [...]`, so it must not validate
        // action *contents* — a route-invalid Σ amount_bps = 200% must still hash.
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions = vec![
            SolanaAction::SplTransfer {
                recipient: Pubkey::new_from_array([20u8; 32]),
                amount_bps: 10_000,
            },
            SolanaAction::SplTransfer {
                recipient: Pubkey::new_from_array([21u8; 32]),
                amount_bps: 10_000,
            },
        ];
        let h = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap();
        assert_ne!(h, [0u8; 32]);
    }

    // -- Golden vector tests (pinned digests, independently computed) --
    // These catch regressions in field order, endianness, or serialization format.
    // Golden vectors computed externally via Python Keccak-256 (pycryptodome).

    #[test]
    fn route_config_hash_golden_vector_empty_actions() {
        // owner=[1]*32, chain_id=42161 (LE), mint=[10]*32, app_id=[5]*32,
        // actions=[] (borsh: u32 LE 0)
        let expected: [u8; 32] = [
            0x2c, 0x8f, 0xf9, 0x4f, 0x33, 0xff, 0x37, 0x3c, 0x25, 0xc5, 0x33, 0xad, 0x96, 0x37,
            0x46, 0x65, 0x8c, 0xf8, 0xc0, 0xa9, 0xac, 0x76, 0xbf, 0xd0, 0x95, 0x9b, 0x5e, 0x85,
            0xee, 0x8c, 0xf7, 0x24,
        ];
        let mint = Pubkey::new_from_array([10u8; 32]);
        let h = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &[]).unwrap();
        assert_eq!(
            h, expected,
            "route_config_hash (empty) golden vector mismatch"
        );
    }

    #[test]
    fn route_config_hash_golden_vector_one_action() {
        // owner=[1]*32, chain_id=42161 (LE), mint=[10]*32, app_id=[5]*32,
        // actions=[SplTransfer { recipient=[20]*32, amount_bps=10000 }]
        // borsh: u32 LE 1 + u8 0 (variant disc) + [20]*32 + u16 LE 10000
        let expected: [u8; 32] = [
            0x0e, 0xe0, 0x16, 0xda, 0xd3, 0xaf, 0xc7, 0x6e, 0xc1, 0xf2, 0x95, 0x60, 0xbb, 0x67,
            0xb0, 0xfc, 0xe5, 0x45, 0x1c, 0x12, 0x4c, 0x5b, 0x01, 0x91, 0x1e, 0xf3, 0x00, 0xde,
            0xdb, 0x5c, 0x79, 0xa9,
        ];
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions = vec![SolanaAction::SplTransfer {
            recipient: Pubkey::new_from_array([20u8; 32]),
            amount_bps: 10000,
        }];
        let h = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap();
        assert_eq!(
            h, expected,
            "route_config_hash (1 action) golden vector mismatch"
        );
    }

    // -- Bounds tests --

    #[test]
    fn route_config_hash_max_actions_ok() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions: Vec<SolanaAction> = (0..crate::constants::MAX_ACTION_IDXS)
            .map(|i| SolanaAction::SplTransfer {
                recipient: Pubkey::new_from_array([i as u8; 32]),
                amount_bps: 5000,
            })
            .collect();
        // Should not panic
        let _ = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions);
    }

    #[test]
    fn route_config_hash_too_many_actions_does_not_panic() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions: Vec<SolanaAction> = (0..=crate::constants::MAX_ACTION_IDXS)
            .map(|i| SolanaAction::SplTransfer {
                recipient: Pubkey::new_from_array([i as u8; 32]),
                amount_bps: 5000,
            })
            .collect();

        let result = std::panic::catch_unwind(|| {
            let _ = route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions);
        });

        assert!(
            result.is_ok(),
            "oversized action_config should return an error, not panic"
        );
    }

    #[test]
    fn route_config_hash_too_many_actions_returns_too_many_action_idxs() {
        let mint = Pubkey::new_from_array([10u8; 32]);
        let actions: Vec<SolanaAction> = (0..=crate::constants::MAX_ACTION_IDXS)
            .map(|i| SolanaAction::SplTransfer {
                recipient: Pubkey::new_from_array([i as u8; 32]),
                amount_bps: 5000,
            })
            .collect();

        let err =
            route_config_hash(&test_owner(), 42161, &mint, &test_app_id(), &actions).unwrap_err();

        match err {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, "TooManyActionIdxs");
            }
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }
}
