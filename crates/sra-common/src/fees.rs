//! Source protocol-fee resolution (v15, EVM parity).
//!
//! Pure helpers shared by the handler (on-chain recompute), tests, and the
//! eventual TS SDK mirror. Semantics track `sra-stx` `BaseHandler.sol`
//! byte-for-byte in observable behavior — including the short-circuit ORDER:
//! sponsor and the `_calculateFees` guards return zero BEFORE the override
//! bound check, and an override supplied while `allow_override == false` is
//! silently ignored (EVM has no revert on that path).

use anchor_lang::prelude::*;

use crate::constants::{BPS_DENOMINATOR, MAX_FEE_BPS, NO_FEE_BPS_OVERRIDE};
use crate::errors::SraError;
use crate::types::{AssetFeeTier, ProtocolFeeConfig};

/// Mirrors EVM `_resolveProtocolFeeBps`. The caller guarantees the app's
/// `ProtocolFeeConfig` exists (EVM `pf.isSet` — on Solana, PDA existence).
///
/// The `<= MAX_FEE_BPS` bound is evaluated ONLY when the override is honored
/// (`bps_override != NO_FEE_BPS_OVERRIDE && allow_override`), exactly like the
/// `require` inside EVM's honored branch.
pub fn resolve_protocol_fee_bps(
    config: &ProtocolFeeConfig,
    tier: Option<&AssetFeeTier>,
    amount: u64,
    bps_override: u16,
) -> Result<u16> {
    if bps_override != NO_FEE_BPS_OVERRIDE && config.allow_override {
        require!(bps_override <= MAX_FEE_BPS, SraError::FeeTooHigh);
        return Ok(bps_override);
    }
    if let Some(tier) = tier {
        return Ok(if amount < tier.threshold {
            tier.below_bps
        } else {
            tier.above_or_equal_bps
        });
    }
    Ok(config.default_fee_bps)
}

/// Mirrors the EVM sponsor skip (`if (!isSponsor) _calculateFees(...)`) plus
/// `_calculateFees` itself: recipient-unset and config-absent return zero
/// before any bps resolution, then `fee = floor(amount * bps / 10_000)`.
///
/// Takes the recipient itself so the "`Pubkey::default()` = fees disabled
/// globally" sentinel stays owned by the fee layer, not re-derived at call
/// sites.
pub fn compute_protocol_fee(
    is_sponsor: bool,
    fee_recipient: &Pubkey,
    config: Option<&ProtocolFeeConfig>,
    tier: Option<&AssetFeeTier>,
    amount: u64,
    bps_override: u16,
) -> Result<u64> {
    if is_sponsor || *fee_recipient == Pubkey::default() {
        return Ok(0);
    }
    let Some(config) = config else {
        return Ok(0);
    };
    let bps = resolve_protocol_fee_bps(config, tier, amount, bps_override)?;
    let fee = (amount as u128)
        .checked_mul(bps as u128)
        .ok_or(SraError::ArithmeticOverflow)?
        / BPS_DENOMINATOR as u128;
    u64::try_from(fee).map_err(|_| error!(SraError::ArithmeticOverflow))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_FEE_BPS;
    use crate::types::{AssetFeeTier, ProtocolFeeConfig};
    use anchor_lang::prelude::Pubkey;

    fn cfg(allow_override: bool, default_fee_bps: u16) -> ProtocolFeeConfig {
        ProtocolFeeConfig {
            app_id: [0x44u8; 32],
            default_fee_bps,
            allow_override,
            bump: 254,
        }
    }

    fn tier(threshold: u64, below_bps: u16, above_or_equal_bps: u16) -> AssetFeeTier {
        AssetFeeTier {
            app_id: [0x44u8; 32],
            mint: Pubkey::new_unique(),
            threshold,
            below_bps,
            above_or_equal_bps,
            bump: 254,
        }
    }

    fn assert_fee_err(result: anchor_lang::Result<u64>, expected: &str) {
        match result.unwrap_err() {
            anchor_lang::error::Error::AnchorError(anchor_err) => {
                assert_eq!(anchor_err.error_name, expected);
            }
            other => panic!("expected AnchorError {expected}, got {other:?}"),
        }
    }

    // --- zero paths short-circuit BEFORE the override bound check (EVM order) ---

    #[test]
    fn sponsor_skips_fee_even_with_out_of_bounds_override() {
        // EVM: `if (!isSponsor) _calculateFees(...)` — resolve is never reached,
        // so an absurd override cannot error.
        let fee = compute_protocol_fee(
            true,
            &Pubkey::new_unique(),
            Some(&cfg(true, 30)),
            None,
            1_000_000,
            60_000,
        )
        .expect("sponsor path must not validate the override");
        assert_eq!(fee, 0);
    }

    #[test]
    fn recipient_unset_returns_zero_before_override_check() {
        let fee = compute_protocol_fee(
            false,
            &Pubkey::default(),
            Some(&cfg(true, 30)),
            None,
            1_000_000,
            60_000,
        )
        .expect("recipient-unset path must not validate the override");
        assert_eq!(fee, 0);
    }

    #[test]
    fn config_absent_returns_zero_even_with_valid_override() {
        // PDA existence == EVM `pf.isSet`; absent app pays nothing.
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            None,
            Some(&tier(10, 1, 2)),
            1_000_000,
            50,
        )
        .expect("config-absent path is fee 0");
        assert_eq!(fee, 0);
    }

    // --- override branch (mirrors `_resolveProtocolFeeBps` exactly) ---

    #[test]
    fn override_honored_when_allowed() {
        let bps = resolve_protocol_fee_bps(&cfg(true, 30), None, 1_000_000, 50)
            .expect("override within bounds");
        assert_eq!(bps, 50);
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            Some(&cfg(true, 30)),
            None,
            1_000_000,
            50,
        )
        .expect("override within bounds");
        assert_eq!(fee, 5_000); // 1_000_000 * 50 / 10_000
    }

    #[test]
    fn override_zero_honored_when_allowed() {
        // `0 <= MAX_FEE_BPS` — a free deposit via override is legal on EVM.
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            Some(&cfg(true, 30)),
            None,
            1_000_000,
            0,
        )
        .expect("zero override is in bounds");
        assert_eq!(fee, 0);
    }

    #[test]
    fn override_ignored_when_disallowed() {
        // EVM silently falls through to tier/default — no revert.
        let bps = resolve_protocol_fee_bps(&cfg(false, 30), None, 1_000_000, 50)
            .expect("disallowed override is ignored, not refused");
        assert_eq!(bps, 30);
    }

    #[test]
    fn out_of_bounds_override_ignored_when_disallowed() {
        // The `require(override_ <= MAX_FEE_BPS)` lives INSIDE the honored
        // branch on EVM; with allowOverride=false it is never evaluated.
        let bps = resolve_protocol_fee_bps(&cfg(false, 30), None, 1_000_000, 60_000)
            .expect("bound check only runs when the override is honored");
        assert_eq!(bps, 30);
    }

    #[test]
    fn override_above_max_errors_when_allowed() {
        assert_fee_err(
            resolve_protocol_fee_bps(&cfg(true, 30), None, 1_000_000, MAX_FEE_BPS + 1)
                .map(u64::from),
            "FeeTooHigh",
        );
    }

    #[test]
    fn override_at_max_honored_when_allowed() {
        let bps = resolve_protocol_fee_bps(&cfg(true, 30), None, 1_000_000, MAX_FEE_BPS)
            .expect("MAX_FEE_BPS is inclusive");
        assert_eq!(bps, MAX_FEE_BPS);
    }

    #[test]
    fn sentinel_override_falls_through_even_when_allowed() {
        let bps = resolve_protocol_fee_bps(&cfg(true, 30), None, 1_000_000, u16::MAX)
            .expect("sentinel means no override");
        assert_eq!(bps, 30);
        assert_eq!(NO_FEE_BPS_OVERRIDE, u16::MAX);
    }

    #[test]
    fn override_bypasses_tier_when_honored() {
        let bps = resolve_protocol_fee_bps(&cfg(true, 30), Some(&tier(10, 11, 22)), 1_000_000, 50)
            .expect("honored override wins over the tier");
        assert_eq!(bps, 50);
    }

    // --- tier branch ---

    #[test]
    fn tier_below_threshold_uses_below_bps() {
        let bps = resolve_protocol_fee_bps(
            &cfg(false, 30),
            Some(&tier(1_000, 11, 22)),
            999,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(bps, 11);
    }

    #[test]
    fn tier_at_threshold_uses_above_or_equal_bps() {
        // EVM: `amount < threshold ? belowBps : aboveOrEqualBps` — equality
        // falls in the `aboveOrEqual` bracket.
        let bps = resolve_protocol_fee_bps(
            &cfg(false, 30),
            Some(&tier(1_000, 11, 22)),
            1_000,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(bps, 22);
    }

    #[test]
    fn tier_above_threshold_uses_above_or_equal_bps() {
        let bps = resolve_protocol_fee_bps(
            &cfg(false, 30),
            Some(&tier(1_000, 11, 22)),
            1_001,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(bps, 22);
    }

    #[test]
    fn default_used_when_no_tier() {
        let bps = resolve_protocol_fee_bps(&cfg(false, 30), None, 1_000_000, NO_FEE_BPS_OVERRIDE)
            .unwrap();
        assert_eq!(bps, 30);
    }

    // --- fee math ---

    #[test]
    fn fee_rounds_down() {
        // 333 * 30 / 10_000 = 0.999 → floor 0 (favors the user, EVM floor div).
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            Some(&cfg(false, 30)),
            None,
            333,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(fee, 0);
    }

    #[test]
    fn fee_at_max_amount_uses_u128_intermediate() {
        // u64::MAX * 1000 overflows u64; the u128 intermediate must carry it.
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            Some(&cfg(false, MAX_FEE_BPS)),
            None,
            u64::MAX,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(fee, ((u64::MAX as u128) * 1_000 / 10_000) as u64);
    }

    #[test]
    fn fee_exact_division() {
        let fee = compute_protocol_fee(
            false,
            &Pubkey::new_unique(),
            Some(&cfg(false, 30)),
            None,
            1_000_000,
            NO_FEE_BPS_OVERRIDE,
        )
        .unwrap();
        assert_eq!(fee, 3_000);
    }
}
