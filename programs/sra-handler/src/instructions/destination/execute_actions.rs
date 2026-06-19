//! Destination action execution.
//!
//! Runs a finalized route's committed `action_config` against the funds staged in
//! its `execution_vault`. Two action variants:
//!
//! * **`SplTransfer`** — static accounts, exact-matched recipient ATA, a
//!   percentage of a start-of-run vault snapshot. No third-party CPI.
//! * **`VerifiedCpi`** — dynamic accounts through the shared verified-CPI engine
//!   (scratch-bound input, pinned source/destination/authority, balance-diff
//!   `≥ min_out` from a signed quote, fail-closed). Only the ed25519 signed-quote
//!   `min_out` source is wired here; the oracle source is not yet executable.
//!
//! Failure model (EVM `try/catch` + atomicity parity, two-pass validate-then-execute):
//! * Bad runtime state of a *handler-pinned* account (a missing/frozen recipient,
//!   scratch, or output ATA, or a frozen source vault) sets `ActionsFailed` and
//!   persists it with no partial movement — the funds stay staged and recoverable
//!   by the route owner.
//! * A structural relayer error (wrong/missing accounts, unpinned program, bad
//!   destination) hard-rejects, leaving the receipt `Finalized` (retryable).
//! * A post-CPI shortfall (`min_out` not met) or any failing CPI hard-reverts the
//!   whole transaction atomically — funds stay in the vault, receipt `Finalized`.
//!   Unlike the EVM `try/catch`, a Solana CPI revert cannot be caught mid-action,
//!   so a failed swap is retryable rather than terminal — strictly more conservative.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token::state::AccountState;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::constants::BPS_DENOMINATOR;
use sra_common::errors::SraError;
use sra_common::quote::{action_context_hash, signed_quote_message};
use sra_common::seeds::{action_config_hash, SEED_EXECUTION, SEED_FILL_RECEIPT, SEED_SCRATCH};
use sra_common::types::{
    ExecutionStatus, FillReceipt, FixedRole, MinOut, SolanaAction, SolanaSraRoute,
    VerifiedCpiRuntime,
};

use super::verified_engine::{
    execute_verified, fixed_slot, token_account_usable, validate_pinned_program,
    validate_template_slots, verify_signed_quote, TokenBalanceCheck, VerifiedCpiCall,
};
use crate::events::{ActionsExecuted, ActionsFailed};

#[derive(Accounts)]
#[instruction(fill_id: [u8; 32])]
pub struct ExecuteActions<'info> {
    /// The route whose committed actions run. Not seed-bound here (that needs
    /// `route_config_hash`, which `execute_actions` does not require): the
    /// `fill_receipt` PDA below is seeded by `route.key()`, so passing any other
    /// route would fail to find a finalized receipt — and per the single-tenant
    /// trust model a route only ever touches its own execution authority.
    pub route: Box<Account<'info, SolanaSraRoute>>,
    /// The replay/lifecycle marker; bound to `route` + `fill_id` by its seeds.
    #[account(
        mut,
        seeds = [SEED_FILL_RECEIPT, route.key().as_ref(), &fill_id],
        bump = fill_receipt.bump,
    )]
    pub fill_receipt: Box<Account<'info, FillReceipt>>,
    #[account(address = route.expected_mint)]
    pub expected_mint: Box<Account<'info, Mint>>,
    /// CHECK: PDA ["execution", route] — the execution-vault authority; validated
    /// by seeds and signs the `SplTransfer` payouts and the vault->scratch move.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    /// CHECK: instructions sysvar, pinned by address; read to locate the signed
    /// price quote backing a `VerifiedCpi` action's `min_out`.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    // Per-action accounts ride in `remaining_accounts`, in action order:
    //   SplTransfer -> [recipient ATA]
    //   VerifiedCpi -> [inner program, then the template slots in order]
}

/// A pass-1 plan entry referencing `remaining_accounts` by index, executed in
/// pass 2 only if no action soft-fails (atomicity).
enum Planned {
    Spl {
        recipient_index: usize,
        amount: u64,
    },
    Verified {
        action_index: usize,
        program_index: usize,
        slots_start: usize,
        slots_len: usize,
        input_amount: u64,
        min_out: u64,
        runtime_index: usize,
    },
}

pub(crate) fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, ExecuteActions<'info>>,
    fill_id: [u8; 32],
    action_config: Vec<SolanaAction>,
    verified_runtimes: Vec<VerifiedCpiRuntime>,
) -> Result<()> {
    // Must be settled and not yet acted on (ActionsFailed is retried via retry_actions).
    require!(
        ctx.accounts.fill_receipt.execution_status == ExecutionStatus::Finalized,
        SraError::InvalidExecutionStatus
    );

    // Re-supplied action list must match the committed digest — no inject/drop/reorder.
    require!(
        action_config_hash(&action_config)? == ctx.accounts.route.action_config_hash,
        SraError::ActionConfigHashMismatch
    );

    let route_key = ctx.accounts.route.key();
    let owner = ctx.accounts.route.owner;
    let expected_mint = ctx.accounts.route.expected_mint;
    let token_program = ctx.accounts.token_program.key();
    let decimals = ctx.accounts.expected_mint.decimals;
    // Snapshot once (not a shrinking live balance): each amount is a fraction of
    // this, so a 100%-summing set moves ~the whole vault.
    let vault_snapshot = ctx.accounts.execution_vault.amount;
    // A frozen source vault would abort every movement — fail soft, not hard-abort.
    let source_frozen = ctx.accounts.execution_vault.state == AccountState::Frozen;

    // Scratch identity is the same for every verified action of this route; derive
    // it once, and only if a VerifiedCpi action is present (keeps the SplTransfer-only
    // path free of the PDA search).
    let scratch = action_config
        .iter()
        .any(|a| matches!(a, SolanaAction::VerifiedCpi { .. }))
        .then(|| {
            let (auth, bump) =
                Pubkey::find_program_address(&[SEED_SCRATCH, route_key.as_ref()], &crate::ID);
            (
                auth,
                bump,
                get_associated_token_address(&auth, &expected_mint),
            )
        });

    let now = Clock::get()?.unix_timestamp;

    // Pass 1: validate accounts (hard reject) + runtime state (soft fail) and plan,
    // moving nothing — so any failure leaves the batch atomic (EVM-parity).
    let mut cursor = 0usize;
    let mut runtime_index = 0usize;
    let mut plan: Vec<Planned> = Vec::new();
    let mut any_action_would_fail = false;

    for (action_index, action) in action_config.iter().enumerate() {
        match action {
            SolanaAction::SplTransfer {
                recipient,
                amount_bps,
            } => {
                require!(
                    cursor < ctx.remaining_accounts.len(),
                    SraError::ActionAccountMismatch
                );
                let info = &ctx.remaining_accounts[cursor];
                let expected = get_associated_token_address(recipient, &expected_mint);
                require!(
                    info.key() == expected && info.is_writable && !info.is_signer,
                    SraError::ActionAccountMismatch
                );
                if source_frozen || !token_account_usable(info, &token_program, &expected_mint) {
                    any_action_would_fail = true;
                } else {
                    plan.push(Planned::Spl {
                        recipient_index: cursor,
                        amount: vault_amount_for_bps(vault_snapshot, *amount_bps),
                    });
                }
                cursor += 1;
            }
            SolanaAction::VerifiedCpi {
                program_id,
                input_mint,
                input_bps,
                account_template,
                output_mint,
                min_out,
                allow_permissioned_output: _,
            } => {
                // Only the ed25519 signed-quote min_out source is wired; oracle is
                // not yet executable (fail-closed, receipt stays Finalized).
                let MinOut::SignedQuote { quoter } = min_out else {
                    return Err(SraError::UnsupportedAction.into());
                };
                // The vault holds expected_mint, so that is what is spent.
                require_keys_eq!(*input_mint, expected_mint, SraError::InvalidInputMint);

                let (scratch_auth, _, scratch_account) =
                    scratch.expect("scratch derived when a VerifiedCpi action is present");

                let slots_len = account_template.slots.len();
                let program_index = cursor;
                let slots_start = cursor + 1;
                let end = slots_start + slots_len;
                require!(
                    end <= ctx.remaining_accounts.len(),
                    SraError::ActionAccountMismatch
                );

                validate_pinned_program(&ctx.remaining_accounts[program_index], program_id)?;

                let slots = &ctx.remaining_accounts[slots_start..end];
                let destination = get_associated_token_address(&owner, output_mint);
                validate_template_slots(
                    slots,
                    account_template,
                    &token_program,
                    &scratch_auth,
                    &scratch_account,
                    &destination,
                )?;

                // One runtime entry per VerifiedCpi action, consumed in order.
                let runtime = verified_runtimes
                    .get(runtime_index)
                    .ok_or(SraError::ActionAccountMismatch)?;
                runtime_index += 1;

                // Soft-fail on handler-pinned-account state (recoverable), like SplTransfer.
                let scratch_info = fixed_slot(slots, account_template, FixedRole::Source)?;
                let dest_info = fixed_slot(slots, account_template, FixedRole::Destination)?;
                let scratch_ok = token_account_usable(scratch_info, &token_program, &expected_mint);
                let dest_ok = token_account_usable(dest_info, &token_program, output_mint);
                if source_frozen || !scratch_ok || !dest_ok {
                    any_action_would_fail = true;
                    cursor = end;
                    continue;
                }

                // Verify the authorized, unexpired signed quote (hard reject).
                let input_amount = vault_amount_for_bps(vault_snapshot, *input_bps);
                let context_hash = action_context_hash(&route_key, &fill_id, action_index as u8);
                let message = signed_quote_message(
                    &context_hash,
                    input_mint,
                    output_mint,
                    input_amount,
                    runtime.min_amount_out,
                    runtime.expiry,
                );
                verify_signed_quote(
                    &ctx.accounts.instructions_sysvar,
                    quoter,
                    &message,
                    runtime.expiry,
                    now,
                )?;

                plan.push(Planned::Verified {
                    action_index,
                    program_index,
                    slots_start,
                    slots_len,
                    input_amount,
                    min_out: runtime.min_amount_out,
                    runtime_index: runtime_index - 1,
                });
                cursor = end;
            }
        }
    }
    // Reject leftover relayer accounts and unused runtime entries.
    require!(
        cursor == ctx.remaining_accounts.len(),
        SraError::ActionAccountMismatch
    );
    require!(
        runtime_index == verified_runtimes.len(),
        SraError::ActionAccountMismatch
    );

    // Soft fail: persist ActionsFailed (return Ok), funds stay staged for retry/recover.
    if any_action_would_fail {
        ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsFailed;
        emit!(ActionsFailed {
            route: route_key,
            fill_id,
        });
        return Ok(());
    }

    // Pass 2: execute the validated plan in action order; PDAs sign their leg.
    let execution_bump = [ctx.bumps.execution_authority];
    let execution_signer: &[&[&[u8]]] = &[&[SEED_EXECUTION, route_key.as_ref(), &execution_bump]];

    // Bind the controlled account infos once; they outlive each engine call.
    let expected_mint_info = ctx.accounts.expected_mint.to_account_info();
    let token_program_info = ctx.accounts.token_program.to_account_info();
    let execution_vault_info = ctx.accounts.execution_vault.to_account_info();
    let execution_authority_info = ctx.accounts.execution_authority.to_account_info();

    for entry in &plan {
        match entry {
            Planned::Spl {
                recipient_index,
                amount,
            } => {
                transfer_checked(
                    CpiContext::new_with_signer(
                        token_program_info.clone(),
                        TransferChecked {
                            from: execution_vault_info.clone(),
                            mint: expected_mint_info.clone(),
                            to: ctx.remaining_accounts[*recipient_index].to_account_info(),
                            authority: execution_authority_info.clone(),
                        },
                        execution_signer,
                    ),
                    *amount,
                    decimals,
                )?;
            }
            Planned::Verified {
                action_index,
                program_index,
                slots_start,
                slots_len,
                input_amount,
                min_out,
                runtime_index,
            } => {
                let (_, scratch_bump, _) =
                    scratch.expect("scratch derived when a VerifiedCpi action is present");
                let scratch_bump = [scratch_bump];
                let scratch_signer: &[&[&[u8]]] =
                    &[&[SEED_SCRATCH, route_key.as_ref(), &scratch_bump]];

                let SolanaAction::VerifiedCpi {
                    account_template, ..
                } = &action_config[*action_index]
                else {
                    unreachable!("plan entry references a VerifiedCpi action");
                };
                let slots = &ctx.remaining_accounts[*slots_start..*slots_start + *slots_len];
                let dest_info = fixed_slot(slots, account_template, FixedRole::Destination)?;

                let call = VerifiedCpiCall {
                    program: &ctx.remaining_accounts[*program_index],
                    inner_data: &verified_runtimes[*runtime_index].inner_data,
                    template: account_template,
                    slots,
                    input_amount: *input_amount,
                    input_decimals: decimals,
                    input_mint: &expected_mint_info,
                    token_program: &token_program_info,
                    execution_vault: &execution_vault_info,
                    execution_authority: &execution_authority_info,
                };
                let post = TokenBalanceCheck {
                    account: dest_info,
                    token_program,
                    min_out: *min_out as u128,
                };
                execute_verified(&call, execution_signer, scratch_signer, &post)?;
            }
        }
    }

    ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsSucceeded;
    emit!(ActionsExecuted {
        route: route_key,
        fill_id,
    });
    Ok(())
}

/// Amount to move for an `amount_bps`-of-vault action: floor(`amount_bps` * vault
/// snapshot / `BPS_DENOMINATOR`).
///
/// The caller snapshots the execution-vault balance once at the start of the run
/// and computes every action's amount off that same snapshot. Because the division
/// floors, a set whose `amount_bps` sum to `BPS_DENOMINATOR` transfers at most the
/// snapshot and can leave a few base units of dust — the remainder stays staged in
/// the vault and is recoverable, never over-spent. `amount_bps <= BPS_DENOMINATOR`
/// is guaranteed upstream (`validate_action_config` at `create_route`, re-bound here
/// by the `action_config_hash` check), so the product is `<= vault_snapshot` and the
/// `u64` cast is lossless.
fn vault_amount_for_bps(vault_snapshot: u64, amount_bps: u16) -> u64 {
    ((vault_snapshot as u128 * amount_bps as u128) / BPS_DENOMINATOR as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_amount_for_bps_full() {
        assert_eq!(vault_amount_for_bps(1_000_000, 10_000), 1_000_000);
    }

    #[test]
    fn vault_amount_for_bps_half() {
        assert_eq!(vault_amount_for_bps(1_000_000, 5_000), 500_000);
    }

    #[test]
    fn vault_amount_for_bps_zero_bps_is_zero() {
        assert_eq!(vault_amount_for_bps(1_000_000, 0), 0);
    }

    #[test]
    fn vault_amount_for_bps_zero_vault_is_zero() {
        assert_eq!(vault_amount_for_bps(0, 10_000), 0);
    }

    #[test]
    fn vault_amount_for_bps_floors() {
        // 100 * 3333 / 10000 = 33.33 -> 33 (floor).
        assert_eq!(vault_amount_for_bps(100, 3_333), 33);
    }

    #[test]
    fn vault_amount_for_bps_split_floors_leaving_dust() {
        let snapshot = 1_000_001u64;
        let a = vault_amount_for_bps(snapshot, 6_000);
        let b = vault_amount_for_bps(snapshot, 4_000);
        assert_eq!(a, 600_000);
        assert_eq!(b, 400_000);
        assert_eq!(
            snapshot - (a + b),
            1,
            "1 base unit of floor dust stays staged"
        );
    }

    #[test]
    fn vault_amount_for_bps_max_vault_full_is_lossless() {
        assert_eq!(vault_amount_for_bps(u64::MAX, 10_000), u64::MAX);
    }
}
