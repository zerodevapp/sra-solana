//! Bidirectional IX attestation between `sra_handler::validate_and_prepare`
//! (IX 1) and `sra_across_adapter::execute_deposit_across` (IX 2) of the
//! Solana→EVM source 2-IX flow.
//!
//! v12 widens the attestation surface beyond the v11 design: routing inputs
//! (`target_token`, `action_idxs`) live in `PrepareParams` instead of a
//! stored route PDA, so each side must byte-compare these fields against the
//! other's instruction data — not just `input_amount` / `output_amount`. The
//! pure verifier here is parameterized by an `IxLayout` and a routing-fields
//! `Routing` value so handler-side and adapter-side wrappers can call it with
//! the appropriate constants and return the appropriate error code on
//! mismatch.
//!
//! This module is host-only logic: it does not touch the `Instructions`
//! sysvar — that lives in each program's `utils/ix_attestation.rs` wrapper,
//! which loads the `Instruction` via `get_instruction_relative` and forwards
//! it here. Keeping the cross-IX comparison free of sysvar reads makes the
//! 11 unit tests below executable in pure Rust.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;

use crate::errors::SraError;

/// Routing fields the bidirectional attestation byte-compares between IX 1
/// and IX 2. Borrowed views so callers can pass references straight from
/// `PrepareParams` (or the adapter equivalent) without copying. `Copy` lets
/// callers (and tests) derive variants via the struct-update syntax.
#[derive(Copy, Clone)]
pub struct Routing<'a> {
    pub input_amount: u64,
    pub output_amount: u64,
    pub target_token: &'a [u8; 32],
    pub action_idxs: &'a [u64],
}

/// Pinning of the *other* IX's program id, instruction discriminator, the
/// expected `sra_config` pubkey, and the byte offsets at which routing fields
/// live inside the other IX's data.
pub struct IxLayout<'a> {
    pub expected_program: &'a Pubkey,
    pub expected_discriminator: &'a [u8; 8],
    pub expected_sra_config: &'a Pubkey,
    pub sra_config_account_index: usize,
    pub data_input_amount_offset: usize,
    pub data_output_amount_offset: usize,
    pub data_target_token_offset: usize,
    pub data_action_idxs_offset: usize,
}

/// Verify that `ix` is the expected counterpart instruction in a 2-IX
/// versioned transaction: program id, discriminator, the `sra_config`
/// account at the layout-fixed slot, and the routing fields all match.
///
/// `on_mismatch` is the error variant the caller wants returned on any
/// failure — handler callers pass `InvalidFollowingInstruction`, adapter
/// callers pass `InvalidPrecedingInstruction`. Returning a single error per
/// side keeps the on-chain log unambiguous.
pub fn verify_ix_routing_payload(
    ix: &Instruction,
    layout: &IxLayout,
    expected: &Routing,
    on_mismatch: SraError,
) -> Result<()> {
    if &ix.program_id != layout.expected_program {
        return Err(error!(on_mismatch));
    }

    if ix.accounts.len() <= layout.sra_config_account_index {
        return Err(error!(on_mismatch));
    }
    if &ix.accounts[layout.sra_config_account_index].pubkey != layout.expected_sra_config {
        return Err(error!(on_mismatch));
    }

    let data = ix.data.as_slice();

    if data.len() < 8 || &data[..8] != layout.expected_discriminator {
        return Err(error!(on_mismatch));
    }

    if read_u64_at(data, layout.data_input_amount_offset)
        .ok_or_else(|| error!(on_mismatch))?
        != expected.input_amount
    {
        return Err(error!(on_mismatch));
    }
    if read_u64_at(data, layout.data_output_amount_offset)
        .ok_or_else(|| error!(on_mismatch))?
        != expected.output_amount
    {
        return Err(error!(on_mismatch));
    }
    if read_bytes32_at(data, layout.data_target_token_offset)
        .ok_or_else(|| error!(on_mismatch))?
        != *expected.target_token
    {
        return Err(error!(on_mismatch));
    }

    let len_off = layout.data_action_idxs_offset;
    let len = read_u32_at(data, len_off).ok_or_else(|| error!(on_mismatch))? as usize;
    if len != expected.action_idxs.len() {
        return Err(error!(on_mismatch));
    }
    let entries_off = len_off + 4;
    let entries_end = entries_off
        .checked_add(len.checked_mul(8).ok_or_else(|| error!(on_mismatch))?)
        .ok_or_else(|| error!(on_mismatch))?;
    if data.len() < entries_end {
        return Err(error!(on_mismatch));
    }
    for (i, expected_v) in expected.action_idxs.iter().enumerate() {
        let off = entries_off + i * 8;
        let v = read_u64_at(data, off).ok_or_else(|| error!(on_mismatch))?;
        if v != *expected_v {
            return Err(error!(on_mismatch));
        }
    }

    Ok(())
}

fn read_u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    if data.len() < end {
        return None;
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&data[offset..end]);
    Some(u32::from_le_bytes(buf))
}

fn read_u64_at(data: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    if data.len() < end {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[offset..end]);
    Some(u64::from_le_bytes(buf))
}

fn read_bytes32_at(data: &[u8], offset: usize) -> Option<[u8; 32]> {
    let end = offset.checked_add(32)?;
    if data.len() < end {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[offset..end]);
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{adapter_ix_layout, handler_ix_layout};
    use anchor_lang::solana_program::instruction::AccountMeta;
    use solana_sha256_hasher::hash as sha256;

    // ---- discriminator regression ---------------------------------------------------------

    #[test]
    fn validate_and_prepare_discriminator_matches_anchor_sighash() {
        let h = sha256(b"global:validate_and_prepare").to_bytes();
        let mut expected = [0u8; 8];
        expected.copy_from_slice(&h[..8]);
        assert_eq!(
            handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            expected,
            "VALIDATE_AND_PREPARE_DISCRIMINATOR drifted from sha256(\"global:validate_and_prepare\")[..8]",
        );
    }

    #[test]
    fn execute_deposit_across_discriminator_matches_anchor_sighash() {
        let h = sha256(b"global:execute_deposit_across").to_bytes();
        let mut expected = [0u8; 8];
        expected.copy_from_slice(&h[..8]);
        assert_eq!(
            adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            expected,
            "EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR drifted from sha256(\"global:execute_deposit_across\")[..8]",
        );
    }

    // ---- helpers --------------------------------------------------------------------------

    fn pk(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn target_token() -> [u8; 32] {
        let mut t = [0u8; 32];
        for (i, x) in t.iter_mut().enumerate() {
            *x = (i as u8).wrapping_add(0xA0);
        }
        t
    }

    /// Encode the routing-fields prefix shared by both IX layouts (after the
    /// 8-byte discriminator). Borsh layout: u64 LE input, u64 LE output,
    /// [u8; 32] target_token, u32 LE action_idxs.len(), n × u64 LE entries.
    fn encode_routing_data(disc: &[u8; 8], r: &Routing) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 8 + 8 + 32 + 4 + r.action_idxs.len() * 8);
        out.extend_from_slice(disc);
        out.extend_from_slice(&r.input_amount.to_le_bytes());
        out.extend_from_slice(&r.output_amount.to_le_bytes());
        out.extend_from_slice(r.target_token);
        out.extend_from_slice(&(r.action_idxs.len() as u32).to_le_bytes());
        for v in r.action_idxs {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Build an `Instruction` that mirrors `validate_and_prepare`'s on-the-
    /// wire shape: 15 accounts, sra_config at slot 2, adapter_program at
    /// slot 9. Other slots are filled with deterministic dummies.
    fn build_handler_ix(
        sra_config: &Pubkey,
        adapter_program: &Pubkey,
        program_id: &Pubkey,
        disc: &[u8; 8],
        r: &Routing,
    ) -> Instruction {
        let mut accounts: Vec<AccountMeta> = (0..15)
            .map(|i| AccountMeta::new_readonly(pk(0xC0 + i as u8), false))
            .collect();
        accounts[handler_ix_layout::ACCT_SRA_CONFIG] = AccountMeta::new(*sra_config, false);
        accounts[handler_ix_layout::ACCT_ADAPTER_PROGRAM] =
            AccountMeta::new_readonly(*adapter_program, false);
        Instruction {
            program_id: *program_id,
            accounts,
            data: encode_routing_data(disc, r),
        }
    }

    /// Build an `Instruction` mirroring `execute_deposit_across`'s expected
    /// on-the-wire shape: at least 4 accounts, sra_config at the layout-
    /// fixed slot. We only need enough slots for the layout-fixed indices.
    fn build_adapter_ix(
        sra_config: &Pubkey,
        program_id: &Pubkey,
        disc: &[u8; 8],
        r: &Routing,
    ) -> Instruction {
        let mut accounts: Vec<AccountMeta> = (0..4)
            .map(|i| AccountMeta::new_readonly(pk(0xD0 + i as u8), false))
            .collect();
        accounts[adapter_ix_layout::ACCT_SRA_CONFIG] = AccountMeta::new(*sra_config, false);
        Instruction {
            program_id: *program_id,
            accounts,
            data: encode_routing_data(disc, r),
        }
    }

    fn handler_layout<'a>(
        adapter_program: &'a Pubkey,
        sra_config: &'a Pubkey,
    ) -> IxLayout<'a> {
        IxLayout {
            expected_program: adapter_program,
            expected_discriminator: &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            expected_sra_config: sra_config,
            sra_config_account_index: adapter_ix_layout::ACCT_SRA_CONFIG,
            data_input_amount_offset: adapter_ix_layout::DATA_INPUT_AMOUNT_OFFSET,
            data_output_amount_offset: adapter_ix_layout::DATA_OUTPUT_AMOUNT_OFFSET,
            data_target_token_offset: adapter_ix_layout::DATA_TARGET_TOKEN_OFFSET,
            data_action_idxs_offset: adapter_ix_layout::DATA_ACTION_IDXS_OFFSET,
        }
    }

    fn adapter_layout<'a>(
        handler_program: &'a Pubkey,
        sra_config: &'a Pubkey,
    ) -> IxLayout<'a> {
        IxLayout {
            expected_program: handler_program,
            expected_discriminator: &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            expected_sra_config: sra_config,
            sra_config_account_index: handler_ix_layout::ACCT_SRA_CONFIG,
            data_input_amount_offset: handler_ix_layout::DATA_INPUT_AMOUNT_OFFSET,
            data_output_amount_offset: handler_ix_layout::DATA_OUTPUT_AMOUNT_OFFSET,
            data_target_token_offset: handler_ix_layout::DATA_TARGET_TOKEN_OFFSET,
            data_action_idxs_offset: handler_ix_layout::DATA_ACTION_IDXS_OFFSET,
        }
    }

    fn assert_err_is(err: anchor_lang::error::Error, expected: SraError) {
        match err {
            anchor_lang::error::Error::AnchorError(e) => {
                let exp_name = match expected {
                    SraError::InvalidFollowingInstruction => "InvalidFollowingInstruction",
                    SraError::InvalidPrecedingInstruction => "InvalidPrecedingInstruction",
                    other => panic!("test only checks attestation errors, got {other:?}"),
                };
                assert_eq!(e.error_name, exp_name);
            }
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    // ---- handler-side: verify_adapter_ix tests --------------------------------------------
    //
    // "verify_adapter_ix" = handler attests that the next instruction is the
    // adapter's execute_deposit_across with matching routing fields. These
    // tests target the pure helper directly using the adapter-IX layout.

    #[test]
    fn verify_adapter_ix_valid_next_passes() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let r = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_adapter_ix(
            &sra_config,
            &adapter_program,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &r,
        );
        let layout = handler_layout(&adapter_program, &sra_config);
        verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidFollowingInstruction)
            .expect("valid IX 2 must pass attestation");
    }

    #[test]
    fn verify_adapter_ix_wrong_program_id_fails() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let r = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        // IX targets the wrong program (a fake foreign adapter).
        let foreign = pk(0x99);
        let ix = build_adapter_ix(
            &sra_config,
            &foreign,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &r,
        );
        let layout = handler_layout(&adapter_program, &sra_config);
        let err = verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidFollowingInstruction)
            .expect_err("wrong program must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_wrong_discriminator_fails() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let r = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let bad_disc = [0u8; 8];
        let ix = build_adapter_ix(&sra_config, &adapter_program, &bad_disc, &r);
        let layout = handler_layout(&adapter_program, &sra_config);
        let err = verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidFollowingInstruction)
            .expect_err("wrong discriminator must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_wrong_sra_config_fails() {
        let sra_config = pk(0x11);
        let other_sra = pk(0x12);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let r = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        // IX 2 references a different sra_config — the attestation must
        // catch this even if everything else looks right.
        let ix = build_adapter_ix(
            &other_sra,
            &adapter_program,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &r,
        );
        let layout = handler_layout(&adapter_program, &sra_config);
        let err = verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidFollowingInstruction)
            .expect_err("wrong sra_config must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_wrong_amounts_fails() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let actual = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_adapter_ix(
            &sra_config,
            &adapter_program,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &actual,
        );
        let layout = handler_layout(&adapter_program, &sra_config);

        // input_amount mismatch
        let mismatched_input = Routing { input_amount: actual.input_amount + 1, ..actual };
        let err =
            verify_ix_routing_payload(&ix, &layout, &mismatched_input, SraError::InvalidFollowingInstruction)
                .expect_err("input amount mismatch must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);

        // output_amount mismatch
        let mismatched_output = Routing { output_amount: actual.output_amount + 1, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched_output,
            SraError::InvalidFollowingInstruction,
        )
        .expect_err("output amount mismatch must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_wrong_target_token_fails() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let actual = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_adapter_ix(
            &sra_config,
            &adapter_program,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &actual,
        );
        let layout = handler_layout(&adapter_program, &sra_config);

        let other_token = [0xFF; 32];
        let mismatched = Routing { target_token: &other_token, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched,
            SraError::InvalidFollowingInstruction,
        )
        .expect_err("target_token mismatch must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_wrong_action_idxs_fails() {
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let actual = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_adapter_ix(
            &sra_config,
            &adapter_program,
            &adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR,
            &actual,
        );
        let layout = handler_layout(&adapter_program, &sra_config);

        // length mismatch: 2 → 3
        let longer: Vec<u64> = vec![3, 7, 11];
        let mismatched_len = Routing { action_idxs: &longer, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched_len,
            SraError::InvalidFollowingInstruction,
        )
        .expect_err("action_idxs length mismatch must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);

        // entry mismatch: same length, different value
        let altered: Vec<u64> = vec![3, 8];
        let mismatched_value = Routing { action_idxs: &altered, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched_value,
            SraError::InvalidFollowingInstruction,
        )
        .expect_err("action_idxs entry mismatch must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    #[test]
    fn verify_adapter_ix_missing_next_fails() {
        // A wrapper that calls `get_instruction_relative(1, sysvar)` returns
        // an error when there is no instruction at the relative offset. Step
        // 12's `validate_and_prepare_standalone_rejected` integration test
        // exercises that path end-to-end via LiteSVM.
        //
        // The pure helper here cannot itself trigger a "missing next" — it
        // takes an already-loaded `&Instruction`. The closest analogue is a
        // `data` slice too short to contain the routing-fields prefix, which
        // an attacker could craft to bypass the discriminator check by
        // truncating early. The helper must reject that precondition.
        let sra_config = pk(0x11);
        let adapter_program = pk(0x22);
        let truncated = Instruction {
            program_id: adapter_program,
            accounts: {
                let mut a: Vec<AccountMeta> = (0..4)
                    .map(|i| AccountMeta::new_readonly(pk(0xD0 + i as u8), false))
                    .collect();
                a[adapter_ix_layout::ACCT_SRA_CONFIG] = AccountMeta::new(sra_config, false);
                a
            },
            data: adapter_ix_layout::EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR.to_vec(), // disc only
        };
        let token = target_token();
        let action_idxs: Vec<u64> = vec![3, 7];
        let r = Routing {
            input_amount: 1_000_000,
            output_amount: 999_500,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let layout = handler_layout(&adapter_program, &sra_config);
        let err = verify_ix_routing_payload(&truncated, &layout, &r, SraError::InvalidFollowingInstruction)
            .expect_err("truncated IX data must reject");
        assert_err_is(err, SraError::InvalidFollowingInstruction);
    }

    // ---- adapter-side: verify_handler_ix tests --------------------------------------------
    //
    // "verify_handler_ix" = adapter attests that the previous instruction is
    // the handler's validate_and_prepare with matching routing fields. Pure
    // helper called with the handler-IX layout.

    #[test]
    fn verify_handler_ix_valid_preceding_passes() {
        let sra_config = pk(0x33);
        let handler_program = pk(0x44);
        let adapter_program = pk(0x55);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![1, 2, 3, 4];
        let r = Routing {
            input_amount: 5_000_000,
            output_amount: 4_975_000,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_handler_ix(
            &sra_config,
            &adapter_program,
            &handler_program,
            &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            &r,
        );
        let layout = adapter_layout(&handler_program, &sra_config);
        verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidPrecedingInstruction)
            .expect("valid IX 1 must pass attestation");
    }

    #[test]
    fn verify_handler_ix_wrong_adapter_program_fails() {
        // The adapter side's program-id check covers the *handler* program id
        // (since it's attesting the handler IX). To represent "wrong adapter
        // program nominated by IX 1", the adapter must additionally verify
        // that the handler IX's `adapter_program` account slot points at
        // *this* adapter (the one currently executing). The pure helper does
        // not do this account-slot check on its own — by design it only
        // checks the slot that holds `sra_config`. Here we exercise the
        // closest pure-helper analogue: the handler IX's program-id field
        // does not match the expected handler program id (i.e., a foreign
        // program is masquerading as the handler in IX 1).
        let sra_config = pk(0x33);
        let handler_program = pk(0x44);
        let foreign = pk(0xAA);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![1, 2, 3, 4];
        let r = Routing {
            input_amount: 5_000_000,
            output_amount: 4_975_000,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        // `build_handler_ix` lets us swap the program_id; pass `foreign` to
        // trigger the program-id branch of the helper.
        let ix = build_handler_ix(
            &sra_config,
            &pk(0x55),
            &foreign,
            &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            &r,
        );
        let layout = adapter_layout(&handler_program, &sra_config);
        let err = verify_ix_routing_payload(&ix, &layout, &r, SraError::InvalidPrecedingInstruction)
            .expect_err("wrong handler/adapter program must reject");
        assert_err_is(err, SraError::InvalidPrecedingInstruction);
    }

    #[test]
    fn verify_handler_ix_wrong_target_token_fails() {
        let sra_config = pk(0x33);
        let handler_program = pk(0x44);
        let adapter_program = pk(0x55);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![1, 2, 3, 4];
        let actual = Routing {
            input_amount: 5_000_000,
            output_amount: 4_975_000,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_handler_ix(
            &sra_config,
            &adapter_program,
            &handler_program,
            &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            &actual,
        );
        let layout = adapter_layout(&handler_program, &sra_config);

        let other_token = [0xCC; 32];
        let mismatched = Routing { target_token: &other_token, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched,
            SraError::InvalidPrecedingInstruction,
        )
        .expect_err("target_token mismatch must reject");
        assert_err_is(err, SraError::InvalidPrecedingInstruction);
    }

    #[test]
    fn verify_handler_ix_wrong_action_idxs_fails() {
        let sra_config = pk(0x33);
        let handler_program = pk(0x44);
        let adapter_program = pk(0x55);
        let token = target_token();
        let action_idxs: Vec<u64> = vec![1, 2, 3, 4];
        let actual = Routing {
            input_amount: 5_000_000,
            output_amount: 4_975_000,
            target_token: &token,
            action_idxs: &action_idxs,
        };
        let ix = build_handler_ix(
            &sra_config,
            &adapter_program,
            &handler_program,
            &handler_ix_layout::VALIDATE_AND_PREPARE_DISCRIMINATOR,
            &actual,
        );
        let layout = adapter_layout(&handler_program, &sra_config);

        // Drop the last entry to change length.
        let shorter: Vec<u64> = vec![1, 2, 3];
        let mismatched_len = Routing { action_idxs: &shorter, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched_len,
            SraError::InvalidPrecedingInstruction,
        )
        .expect_err("action_idxs length mismatch must reject");
        assert_err_is(err, SraError::InvalidPrecedingInstruction);

        // Same length, different first entry.
        let altered: Vec<u64> = vec![99, 2, 3, 4];
        let mismatched_value = Routing { action_idxs: &altered, ..actual };
        let err = verify_ix_routing_payload(
            &ix,
            &layout,
            &mismatched_value,
            SraError::InvalidPrecedingInstruction,
        )
        .expect_err("action_idxs entry mismatch must reject");
        assert_err_is(err, SraError::InvalidPrecedingInstruction);
    }

}
