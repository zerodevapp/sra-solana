//! Minimal Circle CCTP v2 TokenMessengerMinter mock, installed at the real
//! program id in LiteSVM (mirrors the mock-spoke-pool role). `deposit_for_burn`
//! keeps the deployed program's discriminator name, params Borsh layout, and
//! account order — pinned against the vendored CCTP v2 IDL by
//! `tests/mock_cctp.rs` — burns the custody tokens via the passed-through
//! adapter-authority signature, and records what was "sent" in the ephemeral
//! `message_sent_event_data` account.
//!
//! Still needed after the IDL vendoring: the IDL only describes the interface
//! the adapter *encodes*, it isn't executable. LiteSVM source tests need a
//! runnable program at the CCTP id, and the real program requires a fully
//! initialized CCTP environment that only the Surfpool mainnet fork provides.

use anchor_lang::prelude::*;
use anchor_spl::token::{burn, Burn, Mint, Token, TokenAccount};

declare_id!("CCTPV2vPZJS2u2BBsUoscuikbYjnpFmbFsvVuJdgUMQe");

#[error_code]
pub enum MockCctpError {
    #[msg("Burn amount must be positive")]
    InvalidAmount,

    #[msg("Max fee must be less than the burn amount")]
    MaxFeeMustBeLessThanAmount,
}

/// Wire mirror of the deployed `DepositForBurnParams`. Declared independently
/// of the vendored IDL's generated type so the pin test in `tests/mock_cctp.rs`
/// compares two definitions that cannot silently share a layout.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositForBurnParams {
    pub amount: u64,
    pub destination_domain: u32,
    pub mint_recipient: Pubkey,
    pub destination_caller: Pubkey,
    pub max_fee: u64,
    pub min_finality_threshold: u32,
}

/// Mirror of the deployed `MessageSent` account shape (same Anchor account
/// name, so the discriminator matches). The mock stores the Borsh-encoded
/// params it received as the `message` payload rather than a wire-format CCTP
/// message, so source tests can parse exactly what was "sent".
#[account]
#[derive(InitSpace)]
pub struct MessageSent {
    pub rent_payer: Pubkey,
    pub created_at: i64,
    #[max_len(128)]
    pub message: Vec<u8>,
}

#[program]
pub mod mock_cctp {
    use super::*;

    pub fn deposit_for_burn(
        ctx: Context<DepositForBurn>,
        params: DepositForBurnParams,
    ) -> Result<()> {
        require_gt!(params.amount, 0, MockCctpError::InvalidAmount);
        require_gt!(
            params.amount,
            params.max_fee,
            MockCctpError::MaxFeeMustBeLessThanAmount
        );
        // No min-fee/denylist emulation — those real-program checks are
        // covered by the Surfpool E2E suites, not this LiteSVM stand-in.

        // The adapter-authority signature passes through via CPI privilege
        // extension, so the mock burns with `owner` as the authority exactly
        // like the deployed program.
        burn(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Burn {
                    mint: ctx.accounts.burn_token_mint.to_account_info(),
                    from: ctx.accounts.burn_token_account.to_account_info(),
                    authority: ctx.accounts.owner.to_account_info(),
                },
            ),
            params.amount,
        )?;

        let message_sent = &mut ctx.accounts.message_sent_event_data;
        message_sent.rent_payer = ctx.accounts.event_rent_payer.key();
        message_sent.created_at = Clock::get()?.unix_timestamp;
        message_sent.message = params.try_to_vec()?;
        Ok(())
    }
}

/// Account order mirrors the deployed `DepositForBurnContext` (the trailing
/// Anchor `event_cpi` pair the adapter appends rides in `remaining_accounts`).
#[derive(Accounts)]
pub struct DepositForBurn<'info> {
    pub owner: Signer<'info>,
    #[account(mut)]
    pub event_rent_payer: Signer<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub sender_authority_pda: UncheckedAccount<'info>,
    #[account(mut)]
    pub burn_token_account: Box<Account<'info, TokenAccount>>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub denylist_account: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    #[account(mut)]
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub token_messenger: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub remote_token_messenger: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub token_minter: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    #[account(mut)]
    pub local_token: UncheckedAccount<'info>,
    #[account(mut)]
    pub burn_token_mint: Box<Account<'info, Mint>>,
    /// Ephemeral keypair account; `init` requires its co-signature exactly
    /// like the deployed program.
    #[account(init, payer = event_rent_payer, space = 8 + MessageSent::INIT_SPACE)]
    pub message_sent_event_data: Box<Account<'info, MessageSent>>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub message_transmitter_program: UncheckedAccount<'info>,
    /// CHECK: unused in the mock; kept for account-order parity.
    pub token_messenger_minter_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
