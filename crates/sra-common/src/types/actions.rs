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
/// and `SigningAuthority` MUST appear in every template (enforced by
/// `validate_action_config`) so a relayer cannot redirect the input or output.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Debug, InitSpace)]
pub enum FixedRole {
    TokenProgram,
    SigningAuthority,
    Source,
    Destination,
}

/// Minimum-output reference for a `VerifiedCpi`'s post-fee balance-diff gate.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug, InitSpace)]
pub enum MinOut {
    /// Primary: an ed25519-signed quote bound to the fill/action context.
    SignedQuote { quoter: Pubkey },
    /// Fallback: an on-chain price feed with a slippage tolerance.
    Oracle { feed: Pubkey, max_slippage_bps: u16 },
}

impl AccountTemplate {
    fn pins(&self, role: FixedRole) -> bool {
        self.slots.iter().any(|slot| slot == &SlotRole::Fixed(role))
    }
}

/// Validates a route's committed action list — the single fail-fast gate before
/// the list is folded into the route's identity hash at `create_route`.
pub fn validate_action_config(actions: &[SolanaAction]) -> Result<()> {
    require!(
        actions.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );

    let mut spl_transfer_bps: u32 = 0;
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
                // u32 sum of ≤ MAX_ACTION_IDXS u16 values cannot overflow.
                spl_transfer_bps += *amount_bps as u32;
            }
            SolanaAction::VerifiedCpi {
                program_id,
                input_mint,
                output_mint,
                account_template,
                ..
            } => {
                require!(
                    *program_id != Pubkey::default()
                        && *input_mint != Pubkey::default()
                        && *output_mint != Pubkey::default(),
                    SraError::InvalidActionConfig
                );
                // Pin the value-bearing slots so a relayer can't redirect funds.
                require!(
                    account_template.pins(FixedRole::Source)
                        && account_template.pins(FixedRole::Destination)
                        && account_template.pins(FixedRole::SigningAuthority),
                    SraError::InvalidActionConfig
                );
            }
        }
    }

    require!(
        spl_transfer_bps <= BPS_DENOMINATOR as u32,
        SraError::ActionBpsSumExceeded
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
    use anchor_lang::{prelude::*, AnchorDeserialize, AnchorSerialize};

    fn full_template() -> AccountTemplate {
        AccountTemplate {
            slots: vec![
                SlotRole::Fixed(FixedRole::TokenProgram),
                SlotRole::Fixed(FixedRole::SigningAuthority),
                SlotRole::Fixed(FixedRole::Source),
                SlotRole::Fixed(FixedRole::Destination),
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
        let bytes = m.try_to_vec().unwrap();
        assert_eq!(MinOut::try_from_slice(&bytes).unwrap(), m);
    }

    #[test]
    fn min_out_borsh_round_trip_oracle() {
        let m = MinOut::Oracle {
            feed: Pubkey::new_unique(),
            max_slippage_bps: 50,
        };
        let bytes = m.try_to_vec().unwrap();
        assert_eq!(MinOut::try_from_slice(&bytes).unwrap(), m);
    }

    #[test]
    fn validate_action_config_accepts_mixed_valid_config() {
        let actions = vec![valid_spl_transfer(4_000), valid_verified_cpi()];
        assert!(validate_action_config(&actions).is_ok());
    }

    #[test]
    fn validate_action_config_empty_accepted() {
        assert!(validate_action_config(&[]).is_ok());
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
            SraError::ActionBpsSumExceeded.into()
        );
    }

    #[test]
    fn validate_action_config_spl_bps_sum_exactly_10000_accepted() {
        let actions = vec![valid_spl_transfer(6_000), valid_spl_transfer(4_000)];
        assert!(validate_action_config(&actions).is_ok());
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
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_destination_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::SigningAuthority),
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Free,
        ]);
    }

    #[test]
    fn validate_action_config_verified_cpi_template_missing_authority_rejected() {
        assert_template_rejected(&[
            SlotRole::Fixed(FixedRole::Source),
            SlotRole::Fixed(FixedRole::Destination),
            SlotRole::Free,
        ]);
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
            min_out: MinOut::Oracle {
                feed: Pubkey::new_unique(),
                max_slippage_bps: 50,
            },
            allow_permissioned_output: false,
        };
        assert_eq!(
            validate_action_config(&[action]).unwrap_err(),
            SraError::InvalidActionConfig.into()
        );
    }
}
