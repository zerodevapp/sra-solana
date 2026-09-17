//! Minimal Circle CCTP v2 TokenMessengerMinter mock for LiteSVM source tests,
//! installed at the real program id. Runs the burn and records the sent params.

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

/// Independent mirror of the IDL's params type; `tests/mock_cctp.rs` pins the
/// two against each other, so don't merge them.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct DepositForBurnParams {
    pub amount: u64,
    pub destination_domain: u32,
    pub mint_recipient: Pubkey,
    pub destination_caller: Pubkey,
    pub max_fee: u64,
    pub min_finality_threshold: u32,
}

/// Records the sent params (Borsh) for tests to read; not the real wire message.
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
        // min-fee/denylist checks are left to the Surfpool E2E.

        burn(
            CpiContext::new(
                ctx.accounts.token_program.key(),
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
        message_sent.message = borsh::to_vec(&params)?;
        Ok(())
    }
}

/// Account order mirrors the deployed `DepositForBurnContext`; the trailing
/// `event_cpi` pair rides in `remaining_accounts`.
#[derive(Accounts)]
pub struct DepositForBurn<'info> {
    pub owner: Signer<'info>,
    #[account(mut)]
    pub event_rent_payer: Signer<'info>,
    /// CHECK: unused parity placeholder.
    pub sender_authority_pda: UncheckedAccount<'info>,
    #[account(mut)]
    pub burn_token_account: Box<Account<'info, TokenAccount>>,
    /// CHECK: unused parity placeholder.
    pub denylist_account: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    #[account(mut)]
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    pub token_messenger: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    pub remote_token_messenger: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    pub token_minter: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    #[account(mut)]
    pub local_token: UncheckedAccount<'info>,
    #[account(mut)]
    pub burn_token_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = event_rent_payer, space = 8 + MessageSent::INIT_SPACE)]
    pub message_sent_event_data: Box<Account<'info, MessageSent>>,
    /// CHECK: unused parity placeholder.
    pub message_transmitter_program: UncheckedAccount<'info>,
    /// CHECK: unused parity placeholder.
    pub token_messenger_minter_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
