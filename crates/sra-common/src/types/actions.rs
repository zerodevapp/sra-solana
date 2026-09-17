//! Serializable supporting types for the destination action registry.
//!
//! `SolanaAction` itself lives in [`super::destination`]; this module carries the
//! types its `VerifiedCpi` variant references (`AccountTemplate`, `SlotRole`,
//! `FixedRole`, `MinOut`) plus the pure `validate_action_config` checked at
//! `create_route`. Account resolution and amount math are handler-side (they
//! read on-chain accounts) and live with `execute_actions`.

use anchor_lang::prelude::*;

use crate::constants::{BPS_DENOMINATOR, MAX_ACTION_IDXS, MAX_TEMPLATE_SLOTS};
use crate::errors::SraError;
use crate::types::SolanaAction;

/// Ordered account-slot policy for a `VerifiedCpi`'s inner instruction. Slot
/// order matches the inner CPI's account list; each slot is pinned by the
/// handler (`Fixed`) or supplied by the relayer as routing (`Free`).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub struct AccountTemplate {
    #[max_len(MAX_TEMPLATE_SLOTS)]
    pub slots: Vec<SlotRole>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum SlotRole {
    /// Resolved by the handler to a controlled account.
    Fixed(FixedRole),
    /// Relayer-supplied routing account (pool / tick / vault / …).
    Free,
}

/// Handler-controlled roles a `Fixed` slot can take. `Source`, `Destination`,
/// `SigningAuthority`, and `OutputMint` MUST appear in every template (enforced by
/// `validate_action_config`) so a relayer cannot redirect the input or output, and
/// so the handler reads the output mint from a pinned account.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Debug, InitSpace)]
pub enum FixedRole {
    TokenProgram,
    SigningAuthority,
    Source,
    Destination,
    /// The output mint, pinned to the action's committed `output_mint`. The
    /// handler reads its owner (the output token program) and its extensions, so
    /// it can derive the right destination ATA, read a post-fee balance, and gate
    /// permissioned (Token-2022) outputs.
    OutputMint,
}

/// Minimum-output reference for a `VerifiedCpi`'s post-fee balance-diff gate.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum MinOut {
    /// Primary: an ed25519-signed quote bound to the fill/action context.
    SignedQuote { quoter: Pubkey },
    /// Reserved: an on-chain price feed with a slippage tolerance. Not yet
    /// executable — refused at `create_route` until the oracle path is wired
    /// (restore feed/slippage validation when lifting the gate).
    Oracle { feed: Pubkey, max_slippage_bps: u16 },
}

impl AccountTemplate {
    fn pins(&self, role: FixedRole) -> bool {
        self.slots.iter().any(|slot| slot == &SlotRole::Fixed(role))
    }
}

/// Validates a route's committed action list — the single fail-fast gate before
/// the list is folded into the route's identity hash at `create_route`.
///
/// The committed bps (SplTransfer amounts + VerifiedCpi inputs, fractions of
/// one vault snapshot) must sum to exactly 100%: under strands funds past
/// terminal success, over can never execute; only floor dust may remain.
pub fn validate_action_config(actions: &[SolanaAction]) -> Result<()> {
    require!(
        actions.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );

    // u32 sum of ≤ MAX_ACTION_IDXS u16 values cannot overflow.
    let mut total_bps: u32 = 0;
    for action in actions {
        match action {
            SolanaAction::SplTransfer {
                recipient,
                amount_bps,
            } => {
                require!(
                    *recipient != Pubkey::default(),
                    SraError::InvalidActionConfig
                );
                total_bps += *amount_bps as u32;
            }
            SolanaAction::VerifiedCpi {
                program_id,
                input_mint,
                input_bps,
                account_template,
                output_mint,
                min_out,
                ..
            } => {
                require!(
                    *program_id != Pubkey::default()
                        && *input_mint != Pubkey::default()
                        && *output_mint != Pubkey::default(),
                    SraError::InvalidActionConfig
                );
                // Bound the committed template so create/fill hashing + slot
                // resolution stay cheap (#[max_len] only sizes InitSpace).
                require!(
                    account_template.slots.len() <= MAX_TEMPLATE_SLOTS,
                    SraError::TooManyTemplateSlots
                );
                // Pin the value-bearing slots so a relayer can't redirect funds,
                // and the output mint so the handler reads its program/extensions
                // from a committed account.
                require!(
                    account_template.pins(FixedRole::Source)
                        && account_template.pins(FixedRole::Destination)
                        && account_template.pins(FixedRole::SigningAuthority)
                        && account_template.pins(FixedRole::OutputMint),
                    SraError::InvalidActionConfig
                );
                match min_out {
                    // The min-out reference must name a real signer.
                    MinOut::SignedQuote { quoter } => {
                        require!(*quoter != Pubkey::default(), SraError::InvalidActionConfig);
                    }
                    // Not executable by the engine — refuse at commit time,
                    // not after funds land.
                    MinOut::Oracle { .. } => {
                        return Err(SraError::UnsupportedAction.into());
                    }
                }
                total_bps += *input_bps as u32;
            }
        }
    }

    require!(
        total_bps == BPS_DENOMINATOR as u32,
        SraError::InvalidActionBpsSum
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::constants::MAX_TEMPLATE_SLOTS;
    use crate::errors::SraError;
    use crate::types::{
        validate_action_config, AccountTemplate, FixedRole, MinOut, SlotRole, SolanaAction,
    };
    use anchor_lang::{prelude::*, AnchorDeserialize};

    fn full_template() -> AccountTemplate {
        AccountTemplate {
            slots: vec![
                SlotRole::Fixed(FixedRole::TokenProgram),
                SlotRole::Fixed(FixedRole::SigningAuthority),
                SlotRole::Fixed(FixedRole::Source),
                SlotRole::Fixed(FixedRole::Destination),
                SlotRole::Fixed(FixedRole::OutputMint),
                SlotRole::Free,
            ],
        }
    }

    fn valid_verified_cpi() -> SolanaAction {
        SolanaAction::VerifiedCpi {
            program_id: Pubkey::new_unique(),
            input_mint: Pubkey::new_unique(),
            input_bps: 10_000,
            account_template: full_template(),
            output_mint: Pubkey::new_unique(),
            min_out: MinOut::SignedQuote {
                quoter: Pubkey::new_unique(),
            },
            allow_permissioned_output: false,
        }
    }

    fn valid_spl_transfer(amount_bps: u16) -> SolanaAction {
        SolanaAction::SplTransfer {
            recipient: Pubkey::new_unique(),
            amount_bps,
        }
    }

    #[test]
    fn account_template_init_space() {
        // 4 (vec len) + MAX_TEMPLATE_SLOTS * SlotRole (1 disc + 1 FixedRole disc).
        assert_eq!(AccountTemplate::INIT_SPACE, 4 + MAX_TEMPLATE_SLOTS * 2);
    }

    #[test]
    fn min_out_init_space_covers_oracle() {
        // 1 disc + max(SignedQuote 32, Oracle 32+2) = 35.
        assert_eq!(MinOut::INIT_SPACE, 35);
    }

    #[test]
    fn min_out_borsh_round_trip_signed_quote() {
        let m = MinOut::SignedQuote {
            quoter: Pubkey::new_unique(),
        };
        let bytes = borsh::to_vec(&m).unwrap();
        assert_eq!(MinOut::try_from_slice(&bytes).unwrap(), m);
    }

    #[test]
    fn min_out_borsh_round_trip_oracle() {
        let m = MinOut::Oracle {
            feed: Pubkey::new_unique(),
            max_slippage_bps: 50,
        };
        let bytes = borsh::to_vec(&m).unwrap();
        assert_eq!(MinOut::try_from_slice(&bytes).unwrap(), m);
    }

    fn verified_cpi_with_bps(input_bps: u16) -> SolanaAction {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { input_bps: bps, .. } = &mut action {
            *bps = input_bps;
        }
        action
    }

    #[test]
    fn validate_action_config_accepts_mixed_full_consumption_config() {
        let actions = vec![valid_spl_transfer(4_000), verified_cpi_with_bps(6_000)];
        assert!(validate_action_config(&actions).is_ok());
    }

    #[test]
    fn validate_action_config_empty_rejected() {
        // An empty commitment would strand the whole fill past terminal success.
        assert_eq!(
            validate_action_config(&[]).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_too_many_actions_rejected() {
        let actions = vec![valid_spl_transfer(1_000); 5];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::TooManyActionIdxs.into()
        );
    }

    #[test]
    fn validate_action_config_spl_bps_sum_over_10000_rejected() {
        let actions = vec![valid_spl_transfer(6_000), valid_spl_transfer(4_001)];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_spl_bps_sum_under_10000_rejected() {
        let actions = vec![valid_spl_transfer(6_000), valid_spl_transfer(3_999)];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_spl_bps_sum_exactly_10000_accepted() {
        let actions = vec![valid_spl_transfer(6_000), valid_spl_transfer(4_000)];
        assert!(validate_action_config(&actions).is_ok());
    }

    #[test]
    fn validate_action_config_mixed_bps_sum_over_10000_rejected() {
        // Both variants draw from the same snapshot: combined > 100% can never execute.
        let actions = vec![valid_spl_transfer(4_000), valid_verified_cpi()];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_lone_verified_cpi_under_10000_rejected() {
        let actions = vec![verified_cpi_with_bps(5_000)];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_spl_zero_recipient_rejected() {
        let actions = vec![SolanaAction::SplTransfer {
            recipient: Pubkey::default(),
            amount_bps: 5_000,
        }];
        assert_eq!(
            validate_action_config(&actions).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_zero_program_rejected() {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { program_id, .. } = &mut action {
            *program_id = Pubkey::default();
        }
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_zero_input_mint_rejected() {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { input_mint, .. } = &mut action {
            *input_mint = Pubkey::default();
        }
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_zero_output_mint_rejected() {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { output_mint, .. } = &mut action {
            *output_mint = Pubkey::default();
        }
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_source_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Fixed(FixedRole::OutputMint),
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_destination_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::OutputMint),
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_authority_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Fixed(FixedRole::OutputMint),
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_output_mint_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_input_bps_over_10000_rejected() {
        assert_eq!(
            validate_action_config(&[verified_cpi_with_bps(10_001)]).unwrap_err(),
            SraError::InvalidActionBpsSum.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_input_bps_max_boundary_accepted() {
        assert!(validate_action_config(&[verified_cpi_with_bps(10_000)]).is_ok());
    }

    fn pinned_slots_plus_free(free: usize) -> Vec<SlotRole> {
        let mut slots = vec![
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::OutputMint),
        ];
        slots.extend(vec![SlotRole::Free; free]);
        slots
    }

    fn verified_cpi_with_template(slots: Vec<SlotRole>) -> SolanaAction {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi {
            account_template, ..
        } = &mut action
        {
            account_template.slots = slots;
        }
        action
    }

    #[test]
    fn validate_action_config_verified_cpi_oversized_template_rejected() {
        // 4 pins + (MAX - 3) free = MAX + 1 slots (still pins all four).
        let action = verified_cpi_with_template(pinned_slots_plus_free(MAX_TEMPLATE_SLOTS - 3));
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::TooManyTemplateSlots.into()
        );
    }

    #[test]
    fn validate_action_config_verified_cpi_template_at_max_accepted() {
        let action = verified_cpi_with_template(pinned_slots_plus_free(MAX_TEMPLATE_SLOTS - 4));
        assert!(validate_action_config(&[action]).is_ok());
    }

    #[test]
    fn validate_action_config_verified_cpi_zero_quoter_rejected() {
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { min_out, .. } = &mut action {
            *min_out = MinOut::SignedQuote {
                quoter: Pubkey::default(),
            };
        }
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }

    #[test]
    fn validate_action_config_oracle_min_out_rejected_at_create() {
        // Even a well-formed oracle reference is refused at create.
        let mut action = valid_verified_cpi();
        if let SolanaAction::VerifiedCpi { min_out, .. } = &mut action {
            *min_out = MinOut::Oracle {
                feed: Pubkey::new_unique(),
                max_slippage_bps: 50,
            };
        }
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::UnsupportedAction.into()
        );
    }

    fn assert_template_rejected(slots: &[SlotRole]) {
        let action = SolanaAction::VerifiedCpi {
            program_id: Pubkey::new_unique(),
            input_mint: Pubkey::new_unique(),
            input_bps: 10_000,
            account_template: AccountTemplate {
                slots: slots.to_vec(),
            },
            output_mint: Pubkey::new_unique(),
            min_out: MinOut::SignedQuote {
                quoter: Pubkey::new_unique(),
            },
            allow_permissioned_output: false,
        };
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }
}
