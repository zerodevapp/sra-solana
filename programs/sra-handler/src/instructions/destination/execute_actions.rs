//! Destination action execution.
//!
//! Runs a finalized route's committed `action_config` against the staged funds
//! in its `execution_vault`. v1 executes the static `SplTransfer` variant
//! (exact-matched recipient ATA, percentage-of-vault amount); the dynamic
//! `VerifiedCpi` variant is not yet executable and fails closed.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked};
use sra_common::constants::BPS_DENOMINATOR;
use sra_common::errors::SraError;
use sra_common::seeds::{action_config_hash, SEED_EXECUTION, SEED_FILL_RECEIPT};
use sra_common::types::{ExecutionStatus, FillReceipt, SolanaAction, SolanaSraRoute};

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
    /// by seeds and signs the `SplTransfer` payouts.
    #[account(seeds = [SEED_EXECUTION, route.key().as_ref()], bump)]
    pub execution_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = expected_mint,
        associated_token::authority = execution_authority,
    )]
    pub execution_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    // Per-action recipient ATAs ride in `remaining_accounts`, one per
    // `SplTransfer`, in action order — exact-matched against `required_accounts`.
}

pub(crate) fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, ExecuteActions<'info>>,
    fill_id: [u8; 32],
    action_config: Vec<SolanaAction>,
) -> Result<()> {
    // 1. The fill must be settled and not yet acted on: `ActionsSucceeded` is
    //    terminal, and an `ActionsFailed` receipt is re-driven through
    //    `retry_actions`, not here.
    require!(
        ctx.accounts.fill_receipt.execution_status == ExecutionStatus::Finalized,
        SraError::InvalidExecutionStatus
    );

    // 2. The re-supplied action list must match the route's committed digest, so
    //    a relayer can neither inject, drop, nor reorder actions (the
    //    commit-by-hash + pass-as-data model — same shape as the source
    //    `handler_data_hash` check).
    require!(
        action_config_hash(&action_config)? == ctx.accounts.route.action_config_hash,
        SraError::ActionConfigHashMismatch
    );

    let expected_mint = ctx.accounts.route.expected_mint;
    let token_program = ctx.accounts.token_program.key();
    // One balance snapshot for the whole run: every `SplTransfer` takes its
    // `amount_bps` of THIS value, so a set summing to `BPS_DENOMINATOR` drains
    // the vault exactly (computing off a shrinking live balance would not).
    let vault_snapshot = ctx.accounts.execution_vault.amount;

    // Pass 1 — validate every action's accounts against `required_accounts`
    // (a structural relayer error is a hard reject) and the recipient ATA's
    // runtime state (a bad state is a soft `ActionsFailed`), collecting the
    // planned transfers WITHOUT moving any funds. This is what makes the batch
    // atomic: a failure anywhere means no transfer ran (EVM try/catch parity —
    // `_resolveAndExecuteActions` reverts the whole action leg on any failure).
    let mut cursor = 0usize;
    let mut planned_transfers: Vec<(AccountInfo, u64)> = Vec::new();
    let mut any_action_would_fail = false;
    for action in &action_config {
        let metas = required_accounts(action, &expected_mint)?;
        let end = cursor + metas.len();
        require!(
            end <= ctx.remaining_accounts.len(),
            SraError::ActionAccountMismatch
        );
        let supplied = &ctx.remaining_accounts[cursor..end];
        for (info, meta) in supplied.iter().zip(&metas) {
            require!(
                info.key() == meta.pubkey
                    && info.is_writable == meta.is_writable
                    && info.is_signer == meta.is_signer,
                SraError::ActionAccountMismatch
            );
        }
        // Only `SplTransfer` reaches here — `required_accounts` already rejected
        // `VerifiedCpi` with `UnsupportedAction`.
        if let SolanaAction::SplTransfer { amount_bps, .. } = action {
            let recipient_ata = supplied[0].to_account_info();
            if recipient_can_receive(&recipient_ata, &token_program, &expected_mint) {
                planned_transfers.push((
                    recipient_ata,
                    vault_amount_for_bps(vault_snapshot, *amount_bps),
                ));
            } else {
                any_action_would_fail = true;
            }
        }
        cursor = end;
    }
    // No leftover relayer accounts beyond what the actions require.
    require!(
        cursor == ctx.remaining_accounts.len(),
        SraError::ActionAccountMismatch
    );

    // 3. Soft failure: persist `ActionsFailed` and return `Ok` so the status
    //    write survives; the funds stay staged for `retry_actions`/`recover_funds`.
    if any_action_would_fail {
        ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsFailed;
        emit!(ActionsFailed {
            route: ctx.accounts.route.key(),
            fill_id,
        });
        return Ok(());
    }

    // 4. Pass 2 — execute. Every recipient validated and Σ`amount_bps` ≤
    //    `BPS_DENOMINATOR` (enforced at `create_route`, re-bound by the hash
    //    check), so Σ amounts ≤ snapshot and each transfer succeeds. The
    //    execution authority PDA signs.
    let route_key = ctx.accounts.route.key();
    let execution_bump = [ctx.bumps.execution_authority];
    let signer_seeds: &[&[&[u8]]] = &[&[SEED_EXECUTION, route_key.as_ref(), &execution_bump]];
    let decimals = ctx.accounts.expected_mint.decimals;
    for (recipient_ata, amount) in planned_transfers {
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.execution_vault.to_account_info(),
                    mint: ctx.accounts.expected_mint.to_account_info(),
                    to: recipient_ata,
                    authority: ctx.accounts.execution_authority.to_account_info(),
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )?;
    }

    ctx.accounts.fill_receipt.execution_status = ExecutionStatus::ActionsSucceeded;
    emit!(ActionsExecuted {
        route: route_key,
        fill_id,
    });
    Ok(())
}

/// Whether a `transfer_checked` into `info` would succeed: it must be an
/// initialized token account, owned by the route's token program, holding the
/// route's mint, and not frozen. A missing / wrong-owner / frozen ATA returns
/// `false` (the action soft-fails to `ActionsFailed`) rather than aborting.
fn recipient_can_receive(
    info: &AccountInfo,
    token_program: &Pubkey,
    expected_mint: &Pubkey,
) -> bool {
    use anchor_lang::solana_program::program_pack::Pack;
    use anchor_spl::token::spl_token::state::{Account as SplTokenAccount, AccountState};

    if info.owner != token_program {
        return false;
    }
    let Ok(data) = info.try_borrow_data() else {
        return false;
    };
    match SplTokenAccount::unpack(&data) {
        Ok(account) => account.mint == *expected_mint && account.state != AccountState::Frozen,
        Err(_) => false,
    }
}

/// Amount to move for a `SplTransfer`: `amount_bps` of a fixed vault snapshot.
///
/// The caller snapshots the execution-vault balance once at the start of the run
/// and computes every action's amount off that same snapshot, so a set of
/// transfers whose `amount_bps` sum to `BPS_DENOMINATOR` drains the snapshot
/// exactly. `amount_bps <= BPS_DENOMINATOR` is guaranteed upstream
/// (`validate_action_config` at `create_route`, re-bound here by the
/// `action_config_hash` check), so the product is `<= vault_snapshot` and the
/// `u64` cast is lossless.
fn vault_amount_for_bps(vault_snapshot: u64, amount_bps: u16) -> u64 {
    ((vault_snapshot as u128 * amount_bps as u128) / BPS_DENOMINATOR as u128) as u64
}

/// The per-action accounts a relayer must supply in `remaining_accounts`, in
/// order, exact-matched (pubkey + writability + signerness) by `execute_actions`.
///
/// For `SplTransfer` the only relayer-supplied account is the recipient's ATA —
/// the source vault, mint, execution authority, and token program are pinned as
/// top-level instruction accounts, which is stronger than a slice match and
/// keeps the (ALT-mandatory) transaction small. `VerifiedCpi` accounts are
/// outcome-verified, not exact-matched, and its execution path is not built yet,
/// so it returns a typed error here.
fn required_accounts(action: &SolanaAction, expected_mint: &Pubkey) -> Result<Vec<AccountMeta>> {
    match action {
        SolanaAction::SplTransfer { recipient, .. } => Ok(vec![AccountMeta::new(
            get_associated_token_address(recipient, expected_mint),
            false,
        )]),
        SolanaAction::VerifiedCpi { .. } => Err(SraError::UnsupportedAction.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::pubkey::Pubkey;
    use anchor_spl::associated_token::get_associated_token_address;
    use sra_common::errors::SraError;
    use sra_common::types::{AccountTemplate, FixedRole, MinOut, SlotRole, SolanaAction};

    fn spl_transfer(recipient: Pubkey, amount_bps: u16) -> SolanaAction {
        SolanaAction::SplTransfer {
            recipient,
            amount_bps,
        }
    }

    fn verified_cpi() -> SolanaAction {
        SolanaAction::VerifiedCpi {
            program_id: Pubkey::new_unique(),
            input_mint: Pubkey::new_unique(),
            input_bps: 10_000,
            account_template: AccountTemplate {
                slots: vec![
                    SlotRole::Fixed(FixedRole::SigningAuthority),
                    SlotRole::Fixed(FixedRole::Source),
                    SlotRole::Fixed(FixedRole::Destination),
                    SlotRole::Free,
                ],
            },
            output_mint: Pubkey::new_unique(),
            min_out: MinOut::Oracle {
                feed: Pubkey::new_unique(),
                max_slippage_bps: 50,
            },
            allow_permissioned_output: false,
        }
    }

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
    fn vault_amount_for_bps_split_sums_to_snapshot() {
        // The two-pass engine computes every amount off the SAME start-of-run
        // snapshot, so a 60/40 split drains the vault exactly (no shrinkage).
        let snapshot = 1_000_001u64;
        let a = vault_amount_for_bps(snapshot, 6_000);
        let b = vault_amount_for_bps(snapshot, 4_000);
        assert_eq!(a, 600_000);
        assert_eq!(b, 400_000);
        assert!(
            a + b <= snapshot,
            "floored split never exceeds the snapshot"
        );
    }

    #[test]
    fn vault_amount_for_bps_max_vault_full_is_lossless() {
        // bps == BPS_DENOMINATOR returns the snapshot unchanged at the u64 ceiling.
        assert_eq!(vault_amount_for_bps(u64::MAX, 10_000), u64::MAX);
    }

    #[test]
    fn required_accounts_spl_transfer_is_recipient_ata() {
        let recipient = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let metas = required_accounts(&spl_transfer(recipient, 10_000), &mint).unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(
            metas[0].pubkey,
            get_associated_token_address(&recipient, &mint)
        );
        assert!(
            metas[0].is_writable,
            "recipient ATA is the transfer destination"
        );
        assert!(!metas[0].is_signer, "recipient ATA never signs");
    }

    #[test]
    fn required_accounts_verified_cpi_is_unsupported() {
        let err = required_accounts(&verified_cpi(), &Pubkey::new_unique()).unwrap_err();
        assert_eq!(err, SraError::UnsupportedAction.into());
    }
}
