//! Byte layout of `sra_across_adapter::execute_deposit_across` (IX 2 of the
//! Solana→EVM source 2-IX flow), shared between the adapter (the producer) and
//! the handler (which attests to IX 2 from IX 1 via the Instructions sysvar).
//!
//! Step 15 introduces `execute_deposit_across`. To preserve byte-for-byte
//! attestation symmetry with the handler IX, Step 13 fixes the leading layout
//! of the adapter's instruction params to mirror the handler's
//! `PrepareParams`: `input_amount: u64`, `output_amount: u64`,
//! `target_token: [u8; 32]`, `action_idxs: Vec<u64>`. Across-specific fields
//! (`exclusive_relayer`, `fill_deadline_offset`, `exclusivity_period`) follow
//! after the routing-fields block. Step 15 must respect this prefix so the
//! attestation helpers operate without conditional layout logic.
//!
//! `ACCT_SRA_CONFIG` is provisional — Step 15 finalizes the accounts struct
//! and may move the index. The `verify_handler_ix`-side wiring will read this
//! constant, so updating it propagates to both directions.

/// Anchor instruction discriminator for
/// `sra_across_adapter::execute_deposit_across`.
///
/// Computed as `sha256("global:execute_deposit_across")[..8]`. The
/// discriminator-regression test in `crate::ix_attestation::tests` recomputes
/// this value at runtime and asserts equality.
pub const EXECUTE_DEPOSIT_ACROSS_DISCRIMINATOR: [u8; 8] =
    [0x38, 0x6a, 0x44, 0x61, 0x4a, 0xa6, 0xda, 0x3a];

/// Provisional index of `sra_config` in the `execute_deposit_across`
/// `Accounts` struct. Step 15 will finalize the layout; until then this is
/// pinned to slot 1 (after `fee_payer` at slot 0) — the most likely shape and
/// the value Step 13's bidirectional attestation tests assert against.
pub const ACCT_SRA_CONFIG: usize = 1;

/// Offset of `params.input_amount` (u64 LE) inside the IX data, measured from
/// the start of the data including the 8-byte instruction discriminator.
pub const DATA_INPUT_AMOUNT_OFFSET: usize = 8;

/// Offset of `params.output_amount` (u64 LE) inside the IX data. Note: the
/// handler is u64-native; if Step 15 keeps an Across-style U256 surface, it
/// must convert at the CPI boundary so the on-the-wire attestation field stays
/// u64.
pub const DATA_OUTPUT_AMOUNT_OFFSET: usize = 16;

/// Offset of `params.target_token` (`[u8; 32]`) inside the IX data.
pub const DATA_TARGET_TOKEN_OFFSET: usize = 24;

/// Offset of `params.action_idxs` (`Vec<u64>`) inside the IX data. 4-byte LE
/// Borsh length prefix at this offset; entries start at `+4` and span
/// `len * 8` bytes.
pub const DATA_ACTION_IDXS_OFFSET: usize = 56;
