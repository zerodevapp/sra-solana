//! Shared verified-CPI engine. The Solana-only safety model for a PDA-signed CPI
//! over a relayer-supplied account set:
//!
//! 1. Pin the program (committed id, loader-owned, executable).
//! 2. Bound the input: move exactly `input_amount` into a scratch whose authority
//!    is distinct from the vault's, and expose only the scratch — a hostile callee
//!    can take at most that, never the vault.
//! 3. Pin the source/destination/signing-authority slots; the relayer fills only
//!    routing (`Free`) slots, so the output lands in the owner's account.
//! 4. Require `post − pre ≥ min_out` on the pinned output, fail-closed.
//! 5. Sweep the scratch back to the vault; it must end empty.
//!
//! `min_out` is attested by the route-committed quoter co-signing the transaction
//! (checked by the caller before this engine runs), not derived here.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token::{transfer_checked, TransferChecked};
use anchor_spl::token_2022::spl_token_2022::extension::default_account_state::DefaultAccountState;
use anchor_spl::token_2022::spl_token_2022::extension::{
    BaseStateWithExtensions, ExtensionType, StateWithExtensions,
};
use anchor_spl::token_2022::spl_token_2022::state::{
    Account as TokenAccountState, AccountState, Mint,
};

use sra_common::errors::SraError;
use sra_common::types::{AccountTemplate, FixedRole, SlotRole};

use crate::utils::adapter::is_program_owner;

// ---------------------------------------------------------------------------
// Post-condition trait (the only per-action-type difference)
// ---------------------------------------------------------------------------

/// The fail-closed outcome gate. The only implementation today is a post-fee
/// token-balance increase on the route owner's output account.
pub(crate) trait PostCheck {
    /// Measure the tracked quantity now (e.g. the output ATA's balance).
    fn observe(&self) -> Result<u128>;
    /// Reject unless the observed change clears the bar.
    fn assert(&self, pre: u128, post: u128) -> Result<()>;
}

/// `post − pre ≥ min_out` on a pinned token account.
pub(crate) struct TokenBalanceCheck<'a, 'info> {
    pub account: &'a AccountInfo<'info>,
    pub token_program: Pubkey,
    pub min_out: u128,
}

impl PostCheck for TokenBalanceCheck<'_, '_> {
    fn observe(&self) -> Result<u128> {
        // A non-token account at the pinned destination is structural, not soft.
        // The balance is read post-fee (the landed amount, after any Token-2022
        // transfer fee was withheld) on the output token program.
        let token = token_account(self.account, &self.token_program)
            .ok_or(error!(SraError::DestinationNotPinned))?;
        Ok(token.amount as u128)
    }

    fn assert(&self, pre: u128, post: u128) -> Result<()> {
        let gained = post.checked_sub(pre).ok_or(SraError::MinOutNotMet)?;
        require!(gained >= self.min_out, SraError::MinOutNotMet);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// One verified-CPI invocation's inputs. The scratch / signing-authority /
/// destination account infos are inside `slots` (resolved by template role); the
/// vault and its authority are top-level handler accounts.
pub(crate) struct VerifiedCpiCall<'a, 'info> {
    pub program: &'a AccountInfo<'info>,
    pub inner_data: &'a [u8],
    pub template: &'a AccountTemplate,
    pub slots: &'a [AccountInfo<'info>],
    pub input_amount: u64,
    pub input_decimals: u8,
    pub input_mint: &'a AccountInfo<'info>,
    pub token_program: &'a AccountInfo<'info>,
    pub execution_vault: &'a AccountInfo<'info>,
    pub execution_authority: &'a AccountInfo<'info>,
}

/// Run a verified CPI: bound the input into scratch, invoke the pinned program
/// with the resolved account set, verify the outcome, sweep the scratch empty.
///
/// `execution_signer` signs the vault→scratch move (it authorizes the vault);
/// `scratch_signer` signs both the inner CPI and the scratch→vault sweep (it
/// authorizes the scratch, and is distinct from the vault authority so the inner
/// CPI can never move the vault).
pub(crate) fn execute_verified<'info>(
    call: &VerifiedCpiCall<'_, 'info>,
    execution_signer: &[&[&[u8]]],
    scratch_signer: &[&[&[u8]]],
    post: &dyn PostCheck,
) -> Result<()> {
    let scratch = fixed_slot(call.slots, call.template, FixedRole::Source)?;
    let scratch_auth = fixed_slot(call.slots, call.template, FixedRole::SigningAuthority)?;

    // Bound the input: move exactly `input_amount` vault → scratch.
    if call.input_amount > 0 {
        transfer_checked(
            CpiContext::new_with_signer(
                call.token_program.to_account_info(),
                TransferChecked {
                    from: call.execution_vault.to_account_info(),
                    mint: call.input_mint.to_account_info(),
                    to: scratch.to_account_info(),
                    authority: call.execution_authority.to_account_info(),
                },
                execution_signer,
            ),
            call.input_amount,
            call.input_decimals,
        )?;
    }

    // Resolve the account set; snapshot the output before the CPI.
    let metas = build_inner_metas(call.slots, call.template);
    let mut infos: Vec<AccountInfo<'info>> = Vec::with_capacity(call.slots.len() + 1);
    infos.push(call.program.to_account_info());
    infos.extend(call.slots.iter().map(|i| i.to_account_info()));
    let pre = post.observe()?;

    // Invoke the pinned program signed by the scratch authority only.
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: *call.program.key,
        accounts: metas,
        data: call.inner_data.to_vec(),
    };
    invoke_signed(&ix, &infos, scratch_signer)?;

    // Verify the outcome (fail-closed).
    let post_v = post.observe()?;
    post.assert(pre, post_v)?;

    // Sweep any unspent input back to the vault; scratch must end empty.
    let residue = token_amount(scratch, call.token_program.key)?;
    if residue > 0 {
        transfer_checked(
            CpiContext::new_with_signer(
                call.token_program.to_account_info(),
                TransferChecked {
                    from: scratch.to_account_info(),
                    mint: call.input_mint.to_account_info(),
                    to: call.execution_vault.to_account_info(),
                    authority: scratch_auth.to_account_info(),
                },
                scratch_signer,
            ),
            residue,
            call.input_decimals,
        )?;
    }
    require!(
        token_amount(scratch, call.token_program.key)? == 0,
        SraError::ScratchNotEmpty
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Template resolution
// ---------------------------------------------------------------------------

/// Pin the inner program: exact committed id, deployed, loader-owned-executable.
pub(crate) fn validate_pinned_program(program: &AccountInfo, expected: &Pubkey) -> Result<()> {
    require_keys_eq!(*program.key, *expected, SraError::ProgramNotPinned);
    require!(
        !program.data_is_empty() && program.executable && is_program_owner(program.owner),
        SraError::ProgramNotPinned
    );
    Ok(())
}

/// Validate the supplied per-action accounts against the committed template:
/// every `Fixed` slot must equal the handler-resolved account; `Free` slots are
/// unconstrained routing. A mismatched `Destination` is the marquee redirect
/// attack, so it has its own error.
pub(crate) fn validate_template_slots(
    slots: &[AccountInfo],
    template: &AccountTemplate,
    token_program: &Pubkey,
    scratch_auth: &Pubkey,
    scratch: &Pubkey,
    destination: &Pubkey,
    output_mint: &Pubkey,
) -> Result<()> {
    require!(
        slots.len() == template.slots.len(),
        SraError::ActionAccountMismatch
    );
    for (role, info) in template.slots.iter().zip(slots.iter()) {
        match role {
            SlotRole::Fixed(FixedRole::TokenProgram) => {
                require_keys_eq!(info.key(), *token_program, SraError::ActionAccountMismatch);
            }
            SlotRole::Fixed(FixedRole::SigningAuthority) => {
                require_keys_eq!(info.key(), *scratch_auth, SraError::ActionAccountMismatch);
            }
            SlotRole::Fixed(FixedRole::Source) => {
                require_keys_eq!(info.key(), *scratch, SraError::ActionAccountMismatch);
            }
            SlotRole::Fixed(FixedRole::Destination) => {
                require_keys_eq!(info.key(), *destination, SraError::DestinationNotPinned);
            }
            SlotRole::Fixed(FixedRole::OutputMint) => {
                require_keys_eq!(info.key(), *output_mint, SraError::ActionAccountMismatch);
            }
            SlotRole::Free => {}
        }
    }
    Ok(())
}

/// Build the inner-CPI account metas. `Fixed` slots get deterministic flags
/// (only the signing authority is our signer; source/destination are writable);
/// `Free` slots keep the relayer's writability but never sign.
///
/// `Free` slots pass through verbatim — this is also how a Token-2022 transfer-hook
/// output mint's extra accounts reach the inner swap CPI (the relayer supplies them
/// as routing). The executor does not pre-resolve the hook account list: a
/// missing/wrong set makes the inner `transfer_checked` revert, which aborts the
/// whole instruction (fail-closed, funds stay staged and retryable). The post-fee
/// balance-diff is the binding correctness gate regardless of the hook's behavior.
fn build_inner_metas(
    slots: &[AccountInfo],
    template: &AccountTemplate,
) -> Vec<anchor_lang::solana_program::instruction::AccountMeta> {
    use anchor_lang::solana_program::instruction::AccountMeta;
    template
        .slots
        .iter()
        .zip(slots.iter())
        .map(|(role, info)| match role {
            SlotRole::Fixed(FixedRole::TokenProgram) => {
                AccountMeta::new_readonly(info.key(), false)
            }
            SlotRole::Fixed(FixedRole::SigningAuthority) => {
                AccountMeta::new_readonly(info.key(), true)
            }
            SlotRole::Fixed(FixedRole::Source) => AccountMeta::new(info.key(), false),
            SlotRole::Fixed(FixedRole::Destination) => AccountMeta::new(info.key(), false),
            SlotRole::Fixed(FixedRole::OutputMint) => AccountMeta::new_readonly(info.key(), false),
            SlotRole::Free => {
                if info.is_writable {
                    AccountMeta::new(info.key(), false)
                } else {
                    AccountMeta::new_readonly(info.key(), false)
                }
            }
        })
        .collect()
}

/// The first account info filling a given `Fixed` role. `validate_action_config`
/// guarantees Source/Destination/SigningAuthority are each present (≥1), and
/// `validate_template_slots` pins every occurrence to the same resolved account,
/// so the first match is representative even if a role repeats.
pub(crate) fn fixed_slot<'a, 'info>(
    slots: &'a [AccountInfo<'info>],
    template: &AccountTemplate,
    role: FixedRole,
) -> Result<&'a AccountInfo<'info>> {
    template
        .slots
        .iter()
        .zip(slots.iter())
        .find_map(|(r, info)| (*r == SlotRole::Fixed(role)).then_some(info))
        .ok_or(error!(SraError::ActionAccountMismatch))
}

// ---------------------------------------------------------------------------
// Token-account state
// ---------------------------------------------------------------------------

/// `Some(base state)` iff `info` is an initialized token account of `token_program`.
/// Reads via `StateWithExtensions`, so it handles both legacy SPL accounts and
/// Token-2022 accounts carrying extensions (e.g. ImmutableOwner, TransferFeeAmount).
fn token_account(info: &AccountInfo, token_program: &Pubkey) -> Option<TokenAccountState> {
    if info.owner != token_program {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    StateWithExtensions::<TokenAccountState>::unpack(&data)
        .ok()
        .map(|state| state.base)
}

fn token_amount(info: &AccountInfo, token_program: &Pubkey) -> Result<u64> {
    token_account(info, token_program)
        .map(|t| t.amount)
        .ok_or(error!(SraError::ScratchNotEmpty))
}

/// Whether `info` is a usable destination for `mint` under `token_program`:
/// initialized, right mint, not frozen. A bad state soft-fails (recoverable),
/// it does not abort.
pub(crate) fn token_account_usable(
    info: &AccountInfo,
    token_program: &Pubkey,
    mint: &Pubkey,
) -> bool {
    match token_account(info, token_program) {
        Some(t) => t.mint == *mint && t.state != AccountState::Frozen,
        None => false,
    }
}

/// Whether `mint_info` carries an issuer-control extension — permanent delegate,
/// pausable, or default-frozen — that lets a third party move or freeze the held
/// token. Holding such an output is a custody hazard the route owner must opt into
/// (`allow_permissioned_output`). Legacy SPL mints carry no extensions.
pub(crate) fn mint_is_permissioned(mint_info: &AccountInfo) -> Result<bool> {
    if mint_info.owner != &anchor_spl::token_2022::ID {
        return Ok(false);
    }
    let data = mint_info
        .try_borrow_data()
        .map_err(|_| error!(SraError::ActionAccountMismatch))?;
    mint_data_is_permissioned(&data)
}

/// The extension scan over a Token-2022 mint's serialized data, split out from the
/// `AccountInfo` owner check so it is unit-testable against packed mint buffers.
fn mint_data_is_permissioned(data: &[u8]) -> Result<bool> {
    let mint = StateWithExtensions::<Mint>::unpack(data)
        .map_err(|_| error!(SraError::ActionAccountMismatch))?;
    for ext in mint
        .get_extension_types()
        .map_err(|_| error!(SraError::ActionAccountMismatch))?
    {
        match ext {
            ExtensionType::PermanentDelegate | ExtensionType::Pausable => return Ok(true),
            ExtensionType::DefaultAccountState => {
                let das = mint
                    .get_extension::<DefaultAccountState>()
                    .map_err(|_| error!(SraError::ActionAccountMismatch))?;
                if das.state == AccountState::Frozen as u8 {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- mint_is_permissioned (Token-2022 extension scan) ----

    use anchor_lang::solana_program::program_option::COption;
    use anchor_spl::token_2022::spl_token_2022::extension::pausable::PausableConfig;
    use anchor_spl::token_2022::spl_token_2022::extension::permanent_delegate::PermanentDelegate;
    use anchor_spl::token_2022::spl_token_2022::extension::{
        BaseStateWithExtensionsMut, StateWithExtensionsMut,
    };

    /// Pack a Token-2022 mint buffer carrying `exts`, letting `set` fill each
    /// extension after allocation — the same layout the token program writes, so the
    /// scan reads real TLV data (the canonical build order from spl-token-2022).
    fn packed_mint(
        exts: &[ExtensionType],
        set: impl FnOnce(&mut StateWithExtensionsMut<Mint>),
    ) -> Vec<u8> {
        let len = ExtensionType::try_calculate_account_len::<Mint>(exts).unwrap();
        let mut buf = vec![0u8; len];
        {
            let mut state = StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut buf).unwrap();
            set(&mut state);
            state.base = Mint {
                mint_authority: COption::None,
                supply: 0,
                decimals: 6,
                is_initialized: true,
                freeze_authority: COption::None,
            };
            state.pack_base();
            state.init_account_type().unwrap();
        }
        buf
    }

    #[test]
    fn base_token_2022_mint_is_not_permissioned() {
        let data = packed_mint(&[], |_| {});
        assert!(!mint_data_is_permissioned(&data).unwrap());
    }

    #[test]
    fn permanent_delegate_mint_is_permissioned() {
        let data = packed_mint(&[ExtensionType::PermanentDelegate], |m| {
            m.init_extension::<PermanentDelegate>(true).unwrap();
        });
        assert!(mint_data_is_permissioned(&data).unwrap());
    }

    #[test]
    fn pausable_mint_is_permissioned() {
        // The hazard is the issuer's *ability* to pause, so presence of the extension
        // is enough — paused state is not required.
        let data = packed_mint(&[ExtensionType::Pausable], |m| {
            m.init_extension::<PausableConfig>(true).unwrap();
        });
        assert!(mint_data_is_permissioned(&data).unwrap());
    }

    #[test]
    fn default_frozen_mint_is_permissioned() {
        let data = packed_mint(&[ExtensionType::DefaultAccountState], |m| {
            m.init_extension::<DefaultAccountState>(true).unwrap().state =
                AccountState::Frozen as u8;
        });
        assert!(mint_data_is_permissioned(&data).unwrap());
    }

    #[test]
    fn default_initialized_mint_is_not_permissioned() {
        // DefaultAccountState present but == Initialized: no default freeze armed, so
        // not a custody hazard — discriminates the Frozen-state check from mere presence.
        let data = packed_mint(&[ExtensionType::DefaultAccountState], |m| {
            m.init_extension::<DefaultAccountState>(true).unwrap().state =
                AccountState::Initialized as u8;
        });
        assert!(!mint_data_is_permissioned(&data).unwrap());
    }
}
