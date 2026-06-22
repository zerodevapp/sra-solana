//! Destination action execution: run a finalized route's committed `action_config`
//! against funds staged in its `execution_vault`.
//!
//! * `SplTransfer` — exact-matched recipient ATA, a percentage of a start-of-run
//!   vault snapshot. No third-party CPI.
//! * `VerifiedCpi` — dynamic accounts through the shared engine (scratch-bound
//!   input, pinned slots, balance-diff `≥ min_out` from a signed quote). Only the
//!   ed25519 signed-quote source is wired; oracle `min_out` is not yet executable.
//!
//! Failure model (two-pass validate-then-execute): a bad handler-pinned-account
//! state (missing/frozen recipient/scratch/output ATA, frozen vault) → soft
//! `ActionsFailed`, funds staged and owner-recoverable. A structural relayer error
//! → hard reject, receipt stays `Finalized` (retryable). A post-CPI `min_out`
//! shortfall or failing CPI → hard tx revert (Solana can't catch a CPI revert
//! mid-action) — strictly more conservative than EVM's catch.
//!
//! The validate-plan-execute core is `run_committed_actions`, shared with `retry_actions`.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::{
    get_associated_token_address, get_associated_token_address_with_program_id,
};
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
    execute_verified, fixed_slot, mint_is_permissioned, token_account_usable,
    validate_pinned_program, validate_template_slots, verify_signed_quote, TokenBalanceCheck,
    VerifiedCpiCall,
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
        output_token_program: Pubkey,
    },
}

/// Outcome of running committed actions; the caller maps it to status + event.
pub(crate) enum ActionOutcome {
    /// Every action applied.
    Succeeded,
    /// A recoverable per-account problem was found before any transfer (none ran).
    SoftFailed,
}

/// The accounts `run_committed_actions` needs, borrowed identically from
/// `ExecuteActions` and `RetryActions`.
pub(crate) struct ActionAccounts<'a, 'info> {
    pub route: &'a Account<'info, SolanaSraRoute>,
    pub expected_mint: &'a Account<'info, Mint>,
    pub execution_authority: &'a UncheckedAccount<'info>,
    pub execution_vault: &'a Account<'info, TokenAccount>,
    pub token_program: &'a Program<'info, Token>,
    pub instructions_sysvar: &'a UncheckedAccount<'info>,
    pub remaining_accounts: &'a [AccountInfo<'info>],
    pub execution_authority_bump: u8,
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

    let route = ctx.accounts.route.key();
    let accounts = ActionAccounts {
        route: &ctx.accounts.route,
        expected_mint: &ctx.accounts.expected_mint,
        execution_authority: &ctx.accounts.execution_authority,
        execution_vault: &ctx.accounts.execution_vault,
        token_program: &ctx.accounts.token_program,
        instructions_sysvar: &ctx.accounts.instructions_sysvar,
        remaining_accounts: ctx.remaining_accounts,
        execution_authority_bump: ctx.bumps.execution_authority,
    };

    match run_committed_actions(&accounts, fill_id, &action_config, &verified_runtimes)? {
        ActionOutcome::Succeeded => {
            ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsSucceeded;
            emit!(ActionsExecuted { route, fill_id });
        }
        ActionOutcome::SoftFailed => {
            ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsFailed;
            emit!(ActionsFailed { route, fill_id });
        }
    }
    Ok(())
}

/// Verify and run the committed `action_config` against the execution vault.
/// Two-pass: moves nothing until every action passes (atomicity). Returns
/// `SoftFailed` (no transfers) on a recoverable per-account problem, `Succeeded`
/// after the whole batch; hard-rejects return `Err`. Caller sets the receipt status.
pub(crate) fn run_committed_actions<'info>(
    accounts: &ActionAccounts<'_, 'info>,
    fill_id: [u8; 32],
    action_config: &[SolanaAction],
    verified_runtimes: &[VerifiedCpiRuntime],
) -> Result<ActionOutcome> {
    // Re-supplied action list must match the committed digest — no inject/drop/reorder.
    require!(
        action_config_hash(action_config)? == accounts.route.action_config_hash,
        SraError::ActionConfigHashMismatch
    );

    let route_key = accounts.route.key();
    let owner = accounts.route.owner;
    let expected_mint = accounts.route.expected_mint;
    let token_program = accounts.token_program.key();
    let decimals = accounts.expected_mint.decimals;
    // Snapshot once (not a shrinking live balance): each amount is a fraction of
    // this, so a 100%-summing set moves ~the whole vault.
    let vault_snapshot = accounts.execution_vault.amount;
    // A frozen source vault would abort every movement — fail soft, not hard-abort.
    let source_frozen = accounts.execution_vault.state == AccountState::Frozen;

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
                    cursor < accounts.remaining_accounts.len(),
                    SraError::ActionAccountMismatch
                );
                let info = &accounts.remaining_accounts[cursor];
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
                allow_permissioned_output,
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
                    end <= accounts.remaining_accounts.len(),
                    SraError::ActionAccountMismatch
                );

                validate_pinned_program(&accounts.remaining_accounts[program_index], program_id)?;

                let slots = &accounts.remaining_accounts[slots_start..end];

                // The output token program (legacy SPL or Token-2022) is read from
                // the pinned output mint's owner — never assumed, never relayer-set.
                let output_mint_info = fixed_slot(slots, account_template, FixedRole::OutputMint)?;
                require_keys_eq!(
                    output_mint_info.key(),
                    *output_mint,
                    SraError::ActionAccountMismatch
                );
                let output_token_program = *output_mint_info.owner;
                require!(
                    output_token_program == token_program
                        || output_token_program == anchor_spl::token_2022::ID,
                    SraError::ActionAccountMismatch
                );
                // Safety gate: never silently custody a permissioned output
                // (permanent-delegate / pausable / default-frozen) unless the route
                // explicitly opted in.
                require!(
                    *allow_permissioned_output || !mint_is_permissioned(output_mint_info)?,
                    SraError::PermissionedOutputNotAllowed
                );

                let destination = get_associated_token_address_with_program_id(
                    &owner,
                    output_mint,
                    &output_token_program,
                );
                validate_template_slots(
                    slots,
                    account_template,
                    &token_program,
                    &scratch_auth,
                    &scratch_account,
                    &destination,
                    output_mint,
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
                let dest_ok = token_account_usable(dest_info, &output_token_program, output_mint);
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
                    accounts.instructions_sysvar,
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
                    output_token_program,
                });
                cursor = end;
            }
        }
    }
    // Reject leftover relayer accounts and unused runtime entries.
    require!(
        cursor == accounts.remaining_accounts.len(),
        SraError::ActionAccountMismatch
    );
    require!(
        runtime_index == verified_runtimes.len(),
        SraError::ActionAccountMismatch
    );

    // Soft fail (no transfers ran) — caller persists ActionsFailed.
    if any_action_would_fail {
        return Ok(ActionOutcome::SoftFailed);
    }

    // Pass 2: execute the validated plan in action order; PDAs sign their leg.
    let execution_bump = [accounts.execution_authority_bump];
    let execution_signer: &[&[&[u8]]] = &[&[SEED_EXECUTION, route_key.as_ref(), &execution_bump]];

    // Bind the controlled account infos once; they outlive each engine call.
    let expected_mint_info = accounts.expected_mint.to_account_info();
    let token_program_info = accounts.token_program.to_account_info();
    let execution_vault_info = accounts.execution_vault.to_account_info();
    let execution_authority_info = accounts.execution_authority.to_account_info();

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
                            to: accounts.remaining_accounts[*recipient_index].to_account_info(),
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
                output_token_program,
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
                let slots = &accounts.remaining_accounts[*slots_start..*slots_start + *slots_len];
                let dest_info = fixed_slot(slots, account_template, FixedRole::Destination)?;

                let call = VerifiedCpiCall {
                    program: &accounts.remaining_accounts[*program_index],
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
                    token_program: *output_token_program,
                    min_out: *min_out as u128,
                };
                execute_verified(&call, execution_signer, scratch_signer, &post)?;
            }
        }
    }

    Ok(ActionOutcome::Succeeded)
}

/// `floor(amount_bps * vault_snapshot / BPS_DENOMINATOR)`. `amount_bps <= 10_000`
/// is guaranteed upstream (`validate_action_config`, re-bound by the
/// `action_config_hash` check), so the product is `<= vault_snapshot` (lossless
/// `u64` cast); the floor can leave a few base units of recoverable dust.
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
