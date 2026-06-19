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
//! `min_out` comes from an ed25519 signed quote bound to a per-fill/per-action
//! context hash (a port of the EVM signed-pair resolver; ed25519, not secp256k1).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::sysvar::instructions as ix_sysvar;
use anchor_spl::token::{transfer_checked, TransferChecked};
use anchor_spl::token_2022::spl_token_2022::extension::default_account_state::DefaultAccountState;
use anchor_spl::token_2022::spl_token_2022::extension::{
    BaseStateWithExtensions, ExtensionType, StateWithExtensions,
};
use anchor_spl::token_2022::spl_token_2022::state::{
    Account as TokenAccountState, AccountState, Mint,
};

use sra_common::errors::SraError;
use sra_common::quote::SIGNED_QUOTE_MESSAGE_LEN;
use sra_common::types::{AccountTemplate, FixedRole, SlotRole};

use crate::utils::adapter::is_program_owner;

/// The ed25519 native verification program.
const ED25519_PROGRAM_ID: Pubkey = pubkey!("Ed25519SigVerify111111111111111111111111111");

/// Defensive cap on how many transaction instructions the quote scan walks. The
/// v0 transaction account/instruction budget is far below this; the scan also
/// stops at the first out-of-range index.
const MAX_INSTRUCTION_SCAN: usize = 64;

// ed25519 single-signature, self-contained instruction-data layout
// (see solana's ed25519 program). All `*_instruction_index` fields are
// `u16::MAX`, meaning the pubkey/signature/message live in this same instruction.
const ED25519_NUM_SIGNATURES_OFFSET: usize = 0;
const ED25519_OFFSETS_START: usize = 2;
const ED25519_OFFSETS_LEN: usize = 14;
const ED25519_DATA_START: usize = ED25519_OFFSETS_START + ED25519_OFFSETS_LEN; // 16
const ED25519_PUBKEY_LEN: usize = 32;
const ED25519_SIGNATURE_LEN: usize = 64;

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
    let mint = StateWithExtensions::<Mint>::unpack(&data)
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

// ---------------------------------------------------------------------------
// Signed-quote resolution (ed25519)
// ---------------------------------------------------------------------------

/// Confirm the transaction carries an authorized, unexpired signed quote whose
/// bytes equal `expected_message`. Solana verifies the ed25519 precompile over
/// the whole transaction at load, before any instruction runs — so a matching
/// ed25519 instruction *anywhere* in the tx is already verified, and the scan is
/// position-independent by design (restricting it to earlier indices would only
/// reject valid orderings, not add safety). This just locates that verification
/// and checks the signer is the route-committed `quoter` over our message.
pub(crate) fn verify_signed_quote(
    instructions_sysvar: &AccountInfo,
    quoter: &Pubkey,
    expected_message: &[u8; SIGNED_QUOTE_MESSAGE_LEN],
    expiry: i64,
    now: i64,
) -> Result<()> {
    require!(now <= expiry, SraError::PriceQuoteExpired);

    for index in 0..MAX_INSTRUCTION_SCAN {
        let ix = match ix_sysvar::load_instruction_at_checked(index, instructions_sysvar) {
            Ok(ix) => ix,
            Err(_) => break,
        };
        if ix.program_id != ED25519_PROGRAM_ID {
            continue;
        }
        if let Some((signer, message)) = parse_ed25519_self_contained(&ix.data) {
            if signer == *quoter && message == expected_message.as_slice() {
                return Ok(());
            }
        }
    }
    Err(SraError::InvalidPriceSignature.into())
}

/// Extract `(pubkey, message)` from a single-signature, self-contained ed25519
/// instruction in the exact canonical layout the ed25519 instruction builder
/// emits: `[num_sigs=1][pad][offsets(14)][pubkey(32)][signature(64)][message]`,
/// all `*_instruction_index` fields `u16::MAX`, and no trailing bytes. Returns
/// `None` (fail-closed) for anything else — multi-signature data, a
/// cross-instruction reference, non-canonical offsets, or trailing bytes.
///
/// The signature is not re-checked here: the ed25519 native program verifies it
/// transaction-wide before any instruction runs, so a matching record implies the
/// signer signed the message. Pinning the canonical layout means the bytes we read
/// as `(pubkey, message)` are exactly the region the precompile bound to that
/// signature — there is no offset-aliasing slack.
fn parse_ed25519_self_contained(data: &[u8]) -> Option<(Pubkey, &[u8])> {
    if data.len() < ED25519_DATA_START {
        return None;
    }
    // Exactly one signature, fully self-contained.
    if data[ED25519_NUM_SIGNATURES_OFFSET] != 1 {
        return None;
    }
    let o = &data[ED25519_OFFSETS_START..ED25519_DATA_START];
    let u16le = |start: usize| u16::from_le_bytes([o[start], o[start + 1]]);
    let signature_offset = u16le(0) as usize;
    let signature_instruction_index = u16le(2);
    let public_key_offset = u16le(4) as usize;
    let public_key_instruction_index = u16le(6);
    let message_data_offset = u16le(8) as usize;
    let message_data_size = u16le(10) as usize;
    let message_instruction_index = u16le(12);

    // Self-contained only: pubkey, signature, and message must live in this
    // instruction (index == u16::MAX), never referenced from another.
    if signature_instruction_index != u16::MAX
        || public_key_instruction_index != u16::MAX
        || message_instruction_index != u16::MAX
    {
        return None;
    }

    // Canonical, gap-free layout: pubkey then signature then message, with no
    // trailing bytes. This forces the offsets we read to be the ones the
    // precompile verified over.
    let expected_signature_offset = ED25519_DATA_START + ED25519_PUBKEY_LEN;
    let expected_message_offset = expected_signature_offset + ED25519_SIGNATURE_LEN;
    if public_key_offset != ED25519_DATA_START
        || signature_offset != expected_signature_offset
        || message_data_offset != expected_message_offset
        || data.len() != message_data_offset.checked_add(message_data_size)?
    {
        return None;
    }

    let pubkey = Pubkey::try_from(&data[public_key_offset..expected_signature_offset]).ok()?;
    Some((pubkey, &data[message_data_offset..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a self-contained single-signature ed25519 instruction's data, exactly
    /// as solana's `new_ed25519_instruction_with_signature` does.
    fn ed25519_data(pubkey: &[u8; 32], signature: &[u8; 64], message: &[u8]) -> Vec<u8> {
        let public_key_offset = ED25519_DATA_START; // 16
        let signature_offset = public_key_offset + ED25519_PUBKEY_LEN; // 48
        let message_data_offset = signature_offset + 64; // 112

        let mut data = Vec::new();
        data.push(1u8); // num_signatures
        data.push(0u8); // padding
        data.extend_from_slice(&(signature_offset as u16).to_le_bytes());
        data.extend_from_slice(&u16::MAX.to_le_bytes()); // signature_instruction_index
        data.extend_from_slice(&(public_key_offset as u16).to_le_bytes());
        data.extend_from_slice(&u16::MAX.to_le_bytes()); // public_key_instruction_index
        data.extend_from_slice(&(message_data_offset as u16).to_le_bytes());
        data.extend_from_slice(&(message.len() as u16).to_le_bytes());
        data.extend_from_slice(&u16::MAX.to_le_bytes()); // message_instruction_index
        data.extend_from_slice(pubkey);
        data.extend_from_slice(signature);
        data.extend_from_slice(message);
        data
    }

    #[test]
    fn parses_self_contained_pubkey_and_message() {
        let pubkey = [7u8; 32];
        let signature = [9u8; 64];
        let message = b"the signed quote message bytes";
        let data = ed25519_data(&pubkey, &signature, message);
        let (parsed_pk, parsed_msg) = parse_ed25519_self_contained(&data).unwrap();
        assert_eq!(parsed_pk.to_bytes(), pubkey);
        assert_eq!(parsed_msg, message);
    }

    #[test]
    fn rejects_multi_signature_header() {
        let mut data = ed25519_data(&[7u8; 32], &[9u8; 64], b"msg");
        data[0] = 2; // num_signatures != 1
        assert!(parse_ed25519_self_contained(&data).is_none());
    }

    #[test]
    fn rejects_cross_instruction_reference() {
        let mut data = ed25519_data(&[7u8; 32], &[9u8; 64], b"msg");
        // Flip public_key_instruction_index away from u16::MAX.
        let pk_ix_idx_at = ED25519_OFFSETS_START + 6;
        data[pk_ix_idx_at] = 0;
        data[pk_ix_idx_at + 1] = 0;
        assert!(parse_ed25519_self_contained(&data).is_none());
    }

    #[test]
    fn rejects_out_of_bounds_message() {
        let mut data = ed25519_data(&[7u8; 32], &[9u8; 64], b"msg");
        // Inflate message_data_size past the buffer.
        let size_at = ED25519_OFFSETS_START + 10;
        data[size_at] = 0xff;
        data[size_at + 1] = 0xff;
        assert!(parse_ed25519_self_contained(&data).is_none());
    }

    #[test]
    fn rejects_truncated_data() {
        assert!(parse_ed25519_self_contained(&[1u8, 0u8]).is_none());
    }

    #[test]
    fn rejects_non_canonical_pubkey_offset() {
        // A pubkey offset other than the canonical 16 (here pointing past the
        // signature) must be rejected even though it is in-bounds.
        let mut data = ed25519_data(&[7u8; 32], &[9u8; 64], b"message");
        let pk_off_at = ED25519_OFFSETS_START + 4;
        data[pk_off_at..pk_off_at + 2]
            .copy_from_slice(&((ED25519_DATA_START + 1) as u16).to_le_bytes());
        assert!(parse_ed25519_self_contained(&data).is_none());
    }

    #[test]
    fn rejects_trailing_bytes() {
        // Extra bytes after the message break the gap-free canonical length check.
        let mut data = ed25519_data(&[7u8; 32], &[9u8; 64], b"message");
        data.push(0xaa);
        assert!(parse_ed25519_self_contained(&data).is_none());
    }
}
