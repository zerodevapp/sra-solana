//! Generic balance-diff swap mock for destination verified-CPI engine tests.
//!
//! NOT a deploy target (absent from `Anchor.toml`). It stands in for any
//! third-party swap/deposit program (Jupiter, Kamino, …) the verified-CPI engine
//! routes through, exercising the engine's security properties without a real
//! integration:
//!
//! * It **pulls** `in_amount` from the engine-supplied `source` (the scratch
//!   account) using the signing authority the engine grants via `invoke_signed`
//!   — so a `swap` asking for more than the scratch holds fails (the scratch
//!   bound), and asking for less leaves a remainder the engine must sweep.
//! * It **delivers** `out_amount` of the output mint from its own pool to the
//!   engine-pinned `destination` — `out_amount` is caller-controlled, so a test
//!   can under-deliver to exercise the `min_out` floor.
//!
//! Both legs are real SPL `transfer_checked`s, so balances move exactly as a real
//! swap would and the engine's pre/post balance-diff is meaningful.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};

declare_id!("48ua4x3ax2gWbtCBfEdwC9hg8V9FYpG2aFcMNeZ8mK6h");

/// Seed for the mock's pool authority — owns the input sink and output reserve.
pub const POOL_AUTHORITY_SEED: &[u8] = b"pool";

#[program]
pub mod mock_swap {
    use super::*;

    /// Pull `in_amount` of the input mint from `source` into the pool, then
    /// deliver `out_amount` of the output mint from the pool to `destination`.
    pub fn swap(ctx: Context<Swap>, in_amount: u64, out_amount: u64) -> Result<()> {
        // Leg 1: pull input from the scratch source. `signing_authority` is a
        // signer because the caller granted it via `invoke_signed`; the privilege
        // propagates to this nested CPI, so no seeds are needed here.
        if in_amount > 0 {
            transfer_checked(
                CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.source.to_account_info(),
                        mint: ctx.accounts.input_mint.to_account_info(),
                        to: ctx.accounts.pool_input.to_account_info(),
                        authority: ctx.accounts.signing_authority.to_account_info(),
                    },
                ),
                in_amount,
                ctx.accounts.input_mint.decimals,
            )?;
        }

        // Leg 2: deliver output from the pool. The pool authority is this
        // program's PDA, so the mock signs for it.
        if out_amount > 0 {
            let (pool_authority, bump) =
                Pubkey::find_program_address(&[POOL_AUTHORITY_SEED], &crate::ID);
            require_keys_eq!(
                ctx.accounts.pool_authority.key(),
                pool_authority,
                MockSwapError::InvalidPoolAuthority
            );
            let signer_seeds: &[&[&[u8]]] = &[&[POOL_AUTHORITY_SEED, &[bump]]];
            transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.pool_output.to_account_info(),
                        mint: ctx.accounts.output_mint.to_account_info(),
                        to: ctx.accounts.destination.to_account_info(),
                        authority: ctx.accounts.pool_authority.to_account_info(),
                    },
                    signer_seeds,
                ),
                out_amount,
                ctx.accounts.output_mint.decimals,
            )?;
        }

        Ok(())
    }
}

/// Account order matches the engine's committed `AccountTemplate`: the four
/// Fixed slots first (TokenProgram, SigningAuthority, Source, Destination), then
/// the Free routing slots the relayer supplies.
#[derive(Accounts)]
pub struct Swap<'info> {
    pub token_program: Program<'info, Token>,
    /// The engine's scratch authority — a signer via the caller's `invoke_signed`.
    pub signing_authority: Signer<'info>,
    #[account(mut)]
    pub source: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub destination: Box<Account<'info, TokenAccount>>,
    pub input_mint: Box<Account<'info, Mint>>,
    pub output_mint: Box<Account<'info, Mint>>,
    /// CHECK: pool authority PDA (`["pool"]`), validated in-handler before signing.
    pub pool_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub pool_input: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub pool_output: Box<Account<'info, TokenAccount>>,
}

#[error_code]
pub enum MockSwapError {
    #[msg("Pool authority does not match the derived PDA")]
    InvalidPoolAuthority,
}
