//! Byte layout of `sra_handler::validate_and_prepare` (IX 1 of the Solana→EVM
//! source 2-IX flow), shared between the handler (which is the producer) and
//! the adapter (which attests to it from IX 2 via the Instructions sysvar).
//!
//! Constants are pinned to the actual `Accounts` order and `PrepareParams`
//! Borsh layout in
//! `programs/sra-handler/src/instructions/source/validate_and_prepare.rs`. A
//! unit test in `crate::ix_attestation` rebuilds the discriminator from
//! `sha256("global:validate_and_prepare")` and asserts equality, so any rename
//! of the on-chain instruction is caught by the test suite, not by an EVM
//! destination silently rejecting the bridge message.
//!
//! `action_idxs` is a `Vec<u64>` and therefore variable length:
//! `DATA_ACTION_IDXS_OFFSET` points at the 4-byte little-endian length prefix
//! that Borsh writes for vectors. The first entry begins at
//! `DATA_ACTION_IDXS_OFFSET + 4`, and each subsequent entry advances by 8.

/// Anchor instruction discriminator for `sra_handler::validate_and_prepare`.
///
/// Computed as `sha256("global:validate_and_prepare")[..8]`. The
/// discriminator-regression test in `crate::ix_attestation::tests` recomputes
/// this value at runtime and asserts equality.
pub const VALIDATE_AND_PREPARE_DISCRIMINATOR: [u8; 8] =
    [0x5e, 0xf5, 0xbf, 0xb6, 0xb9, 0x2b, 0xf0, 0xaf];

/// Index of `sra_config` in the `validate_and_prepare` `Accounts` struct.
/// Order: fee_payer (0), owner (1), **sra_config (2)**, mint_nonce (3),
/// source_mint (4), owner_token_account (5), sra_vault (6),
/// adapter_authority (7), adapter_custody (8), adapter_program (9),
/// address_book_entry (10), token_program (11), associated_token_program (12),
/// system_program (13), instructions_sysvar (14).
pub const ACCT_SRA_CONFIG: usize = 2;

/// Index of `adapter_program` in the `validate_and_prepare` `Accounts` struct.
/// Used by the adapter's `verify_handler_ix` to confirm the preceding handler
/// IX nominated the adapter program currently executing.
pub const ACCT_ADAPTER_PROGRAM: usize = 9;

/// Offset of `params.input_amount` (u64 LE) inside the IX data, measured from
/// the start of the data including the 8-byte instruction discriminator.
pub const DATA_INPUT_AMOUNT_OFFSET: usize = 8;

/// Offset of `params.output_amount` (u64 LE) inside the IX data.
pub const DATA_OUTPUT_AMOUNT_OFFSET: usize = 16;

/// Offset of `params.target_token` (`[u8; 32]`) inside the IX data.
pub const DATA_TARGET_TOKEN_OFFSET: usize = 24;

/// Offset of `params.action_idxs` (`Vec<u64>`) inside the IX data. The 4-byte
/// little-endian Borsh length prefix lives at this offset; entries start at
/// `+4` and span `len * 8` bytes.
pub const DATA_ACTION_IDXS_OFFSET: usize = 56;
