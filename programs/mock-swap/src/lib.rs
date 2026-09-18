//! Generic balance-diff swap mock for verified-CPI engine tests (not a deploy
//! target). Stands in for any third-party swap/deposit the engine routes through.
//! Two real `transfer_checked` legs: pull `in_amount` from the scratch `source`
//! (signing authority granted by the engine's `invoke_signed`, so over-pulling
//! fails the scratch bound), then deliver `out_amount` from its pool to the pinned
//! `destination` (`out_amount` is caller-controlled, to exercise the `min_out`
//! floor).
//!
//! The two legs carry separate token programs (`token_program` for the input leg,
//! `output_token_program` for the output leg) and `token_interface` account types,
//! so a legacy-SPL input can swap to a Token-2022 output — the cross-program shape
//! every real swap into a Token-2022 mint takes.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

declare_id!("9vGR2dTimYrEswj5fhQQNuTRjqHN2mwby2V77DFr6rTZ");

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
                    ctx.accounts.token_program.key(),
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
        // program's PDA, so the mock signs for it. The output leg uses its own
        // token program, so the output mint can be Token-2022.
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
                    ctx.accounts.output_token_program.key(),
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

/// Account order matches the engine's committed `AccountTemplate`: the five Fixed
/// slots first (TokenProgram, SigningAuthority, Source, Destination, OutputMint),
/// then the Free routing slots the relayer supplies (output token program, input
/// mint, pool accounts).
#[derive(Accounts)]
pub struct Swap<'info> {
    /// Input-leg token program (the bridged token is legacy SPL).
    pub token_program: Interface<'info, TokenInterface>,
    /// The engine's scratch authority — a signer via the caller's `invoke_signed`.
    pub signing_authority: Signer<'info>,
    #[account(mut)]
    pub source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub output_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Output-leg token program (may be Token-2022).
    pub output_token_program: Interface<'info, TokenInterface>,
    pub input_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: pool authority PDA (`["pool"]`), validated in-handler before signing.
    pub pool_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub pool_input: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub pool_output: Box<InterfaceAccount<'info, TokenAccount>>,
}

#[error_code]
pub enum MockSwapError {
    #[msg("Pool authority does not match the derived PDA")]
    InvalidPoolAuthority,
}
