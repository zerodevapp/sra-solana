//! Byte-identical Rust reimplementation of Solidity's `abi.encode(...)` for
//! the SRA cross-chain `Message`.
//!
//! # Why hand-rolled
//!
//! The EVM `SRAHandler.onFill` decodes the cross-chain payload via
//! `abi.decode(message, (address, address, uint256, Message))`. Any divergence
//! between Solana and Solidity encoding would make EVM destinations reject
//! the message — stranding funds. This module produces output that matches
//! `abi.encode(adapter, handler, sourceChainId, message)` byte-for-byte.
//!
//!
//! # Reference Solidity types
//!
//! ```text
//! struct TokenParam   { address token; uint256 amount; }
//! struct DepositInfo  { TokenParam depositParam; uint256 targetChainId;
//!                       TokenParam targetParam; bytes extra; }
//! struct Message      { bytes32 account; bytes initCode; DepositInfo depositInfo;
//!                       uint256[] actionIdxs; bytes32 appId; }
//! // Top level: abi.encode(address adapter, address handler, uint256 sourceChainId, Message)
//! ```

use anchor_lang::prelude::Pubkey;
use solana_keccak_hasher::hash as keccak_hash;

// ---------------------------------------------------------------------------
// Plain-struct mirrors of the Solidity types, keyed for byte-faithful encoding.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenParam {
    /// EVM `address` (20 bytes). Left-padded to 32 bytes during encoding.
    pub token: [u8; 20],
    /// EVM `uint256`. Big-endian bytes.
    pub amount: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepositInfo {
    pub deposit_param: TokenParam,
    /// EVM `uint256`. Big-endian bytes.
    pub target_chain_id: [u8; 32],
    pub target_param: TokenParam,
    /// Opaque bridge-specific bytes. Encoded as Solidity `bytes` (length + data + padding).
    pub extra: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Solidity `bytes32 account`. Solana pubkeys use all 32 bytes; EVM
    /// addresses are left-padded with 12 zero bytes.
    pub account: [u8; 32],
    /// Solidity `bytes initCode`.
    pub init_code: Vec<u8>,
    pub deposit_info: DepositInfo,
    /// Solidity `uint256[] actionIdxs`. Each entry is big-endian.
    pub action_idxs: Vec<[u8; 32]>,
    /// Solidity `bytes32 appId`.
    pub app_id: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum AbiError {
    /// A `bytes32` value had non-zero bytes in the upper 12 positions, so it
    /// cannot be narrowed to a 20-byte EVM `address`.
    InvalidEvmAddress,
}

// ---------------------------------------------------------------------------
// Word writers / primitive encoders
// ---------------------------------------------------------------------------

/// Size of an EVM ABI word in bytes. Every head slot, length prefix, and
/// dynamic-tail entry is padded to this boundary.
const WORD: usize = 32;

fn u256_from_usize(v: usize) -> [u8; WORD] {
    let mut r = [0u8; WORD];
    let bytes = (v as u64).to_be_bytes();
    r[WORD - bytes.len()..].copy_from_slice(&bytes);
    r
}

/// Encode a `u64` as a big-endian `uint256` word.
///
/// The rest of the codebase (e.g. `SraConfig.target_chain_id`,
/// `MintNonce.nonce`, per-deposit `action_idxs`) stores chain IDs, nonces,
/// amounts, and action indexes as `u64`. Use this helper to widen them into
/// the `[u8; 32]` representation the ABI structs require — it places the
/// value in the correct (rightmost) bytes and zero-fills the upper 24.
pub fn u256_from_u64(v: u64) -> [u8; WORD] {
    let mut r = [0u8; WORD];
    r[WORD - 8..].copy_from_slice(&v.to_be_bytes());
    r
}

/// Big-endian `uint256` word from a `u128`.
pub fn u256_from_u128(v: u128) -> [u8; WORD] {
    let mut r = [0u8; WORD];
    r[WORD - 16..].copy_from_slice(&v.to_be_bytes());
    r
}

fn address_word(addr: &[u8; 20]) -> [u8; WORD] {
    let mut r = [0u8; WORD];
    r[WORD - 20..].copy_from_slice(addr);
    r
}

/// Encode a Solidity `bytes` value: one-word length + data + zero padding to a
/// word boundary. Zero-length input produces exactly one zero word.
fn encode_bytes_dynamic(data: &[u8]) -> Vec<u8> {
    let pad = (WORD - data.len() % WORD) % WORD;
    let mut out = Vec::with_capacity(WORD + data.len() + pad);
    out.extend_from_slice(&u256_from_usize(data.len()));
    out.extend_from_slice(data);
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// Encode a Solidity `uint256[]`: one-word length followed by each entry as a
/// full word.
fn encode_uint256_array_dynamic(arr: &[[u8; 32]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(WORD + arr.len() * WORD);
    out.extend_from_slice(&u256_from_usize(arr.len()));
    for e in arr {
        out.extend_from_slice(e);
    }
    out
}

// ---------------------------------------------------------------------------
// Struct encoders — each returns the full encoded blob, including the dynamic
// tail. Offsets are written relative to the struct's own base address so the
// blob can be nested anywhere via a single offset pointer.
// ---------------------------------------------------------------------------

/// `TokenParam` is fully static: `address` (padded to 32) then `uint256`.
fn encode_token_param_static(tp: &TokenParam) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&address_word(&tp.token));
    out[32..].copy_from_slice(&tp.amount);
    out
}

/// `DepositInfo` is dynamic (contains `bytes extra`). Head = 6 words
/// (two flattened TokenParams + targetChainId + offset to extra). Tail =
/// encoded `extra`. Offsets are relative to the start of this blob.
fn encode_deposit_info(di: &DepositInfo) -> Vec<u8> {
    const HEAD_WORDS: usize = 6;
    let head_size = HEAD_WORDS * WORD;

    let tail = encode_bytes_dynamic(&di.extra);

    let mut out = Vec::with_capacity(head_size + tail.len());
    out.extend_from_slice(&encode_token_param_static(&di.deposit_param));
    out.extend_from_slice(&di.target_chain_id);
    out.extend_from_slice(&encode_token_param_static(&di.target_param));
    out.extend_from_slice(&u256_from_usize(head_size)); // offset to extra
    out.extend(tail);
    out
}

/// `Message` is dynamic (contains `bytes initCode`, `DepositInfo` and
/// `uint256[]`). Head = 5 words (account + 3 offsets + appId). Tail =
/// initCode || depositInfo || actionIdxs, each encoded as a blob. Offsets are
/// relative to the start of this blob.
fn encode_message(msg: &Message) -> Vec<u8> {
    const HEAD_WORDS: usize = 5;
    let head_size = HEAD_WORDS * WORD;

    let init_code_enc = encode_bytes_dynamic(&msg.init_code);
    let deposit_info_enc = encode_deposit_info(&msg.deposit_info);
    let action_idxs_enc = encode_uint256_array_dynamic(&msg.action_idxs);

    let off_init_code = head_size;
    let off_deposit_info = off_init_code + init_code_enc.len();
    let off_action_idxs = off_deposit_info + deposit_info_enc.len();

    let total_size = off_action_idxs + action_idxs_enc.len();
    let mut out = Vec::with_capacity(total_size);
    out.extend_from_slice(&msg.account);
    out.extend_from_slice(&u256_from_usize(off_init_code));
    out.extend_from_slice(&u256_from_usize(off_deposit_info));
    out.extend_from_slice(&u256_from_usize(off_action_idxs));
    out.extend_from_slice(&msg.app_id);

    out.extend(init_code_enc);
    out.extend(deposit_info_enc);
    out.extend(action_idxs_enc);
    out
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Canonical `abi.encode(address adapter, address handler, uint256 sourceChainId, Message message)`.
///
/// Top-level head is `(adapter, handler, sourceChainId, offset_to_message)`
/// — the first three are static, `Message` is dynamic so its offset points at
/// the start of the tail (always `4 * 32 = 128`).
pub fn abi_encode_sra_message(
    adapter: &[u8; 20],
    handler: &[u8; 20],
    source_chain_id: &[u8; 32],
    message: &Message,
) -> Vec<u8> {
    const HEAD_WORDS: usize = 4;
    let head_size = HEAD_WORDS * WORD;

    let message_enc = encode_message(message);
    let mut out = Vec::with_capacity(head_size + message_enc.len());
    out.extend_from_slice(&address_word(adapter));
    out.extend_from_slice(&address_word(handler));
    out.extend_from_slice(source_chain_id);
    out.extend_from_slice(&u256_from_usize(head_size));
    out.extend(message_enc);
    out
}

/// Encodes the replay pre-image `abi.encode(uint256 sourceChainId, Message message)`.
///
/// `SRAHandler._triggerTargetAction` computes
/// `keccak256(abi.encode(sourceChainId, message))` and stores it in
/// `processedMessages[]` (SRAHandler.sol:185) for replay protection. This is
/// a *different* encoding shape from `abi_encode_sra_message` — no
/// `adapter`/`handler` prefix — so a caller that wants the replay key must
/// use this function, not the top-level encoder.
///
/// Top-level head is `(sourceChainId, offset_to_message)` — one static word
/// plus the dynamic `Message` offset (always `2 * 32 = 64`).
pub fn abi_encode_for_replay(source_chain_id: &[u8; 32], message: &Message) -> Vec<u8> {
    const HEAD_WORDS: usize = 2;
    let head_size = HEAD_WORDS * WORD;

    let message_enc = encode_message(message);
    let mut out = Vec::with_capacity(head_size + message_enc.len());
    out.extend_from_slice(source_chain_id);
    out.extend_from_slice(&u256_from_usize(head_size));
    out.extend(message_enc);
    out
}

/// Convenience: `keccak256(abi_encode_for_replay(source_chain_id, message))`.
/// Returns the 32-byte replay key EVM destinations index on.
pub fn replay_hash(source_chain_id: &[u8; 32], message: &Message) -> [u8; 32] {
    let pre_image = abi_encode_for_replay(source_chain_id, message);
    keccak_hash(&pre_image).to_bytes()
}

/// Builds the SRA v1 extra envelope: `abi.encode(target_adapter, abi.encode(nonce, bytes("")))`.
///
/// The outer tuple has one static element (address) and one dynamic element
/// (bytes), giving head = 2 words. The inner tuple is `abi.encode(uint256 nonce, bytes(""))`
/// which has head = 2 words (nonce + offset-to-empty-bytes) and a 1-word tail
/// (length 0) — 96 bytes total.
pub fn build_extra_envelope(target_adapter: &[u8; 20], nonce: u64) -> Vec<u8> {
    // Inner abi.encode(nonce, bytes(""))
    let mut inner = Vec::with_capacity(3 * WORD);
    inner.extend_from_slice(&u256_from_u64(nonce));
    inner.extend_from_slice(&u256_from_usize(2 * WORD)); // offset to empty bytes
    inner.extend_from_slice(&[0u8; WORD]); // empty bytes length

    // Outer abi.encode(address, bytes)
    let bytes_tail = encode_bytes_dynamic(&inner);
    let mut out = Vec::with_capacity(2 * WORD + bytes_tail.len());
    out.extend_from_slice(&address_word(target_adapter));
    out.extend_from_slice(&u256_from_usize(2 * WORD)); // offset to bytes
    out.extend(bytes_tail);
    out
}

/// Source token identifier used in `depositParam.token` for cross-chain replay
/// uniqueness. `keccak256(mint)[12..32]` — matches EVM `address(uint160(uint256(hash)))`.
pub fn source_token_hash(mint: &Pubkey) -> [u8; 20] {
    let h = keccak_hash(mint.as_ref()).to_bytes();
    let mut out = [0u8; 20];
    out.copy_from_slice(&h[12..32]);
    out
}

/// Raw 32-byte pubkey representation (no padding).
pub fn pubkey_to_bytes32(pk: &Pubkey) -> [u8; 32] {
    pk.to_bytes()
}

/// Left-pad a 20-byte EVM address with 12 zero bytes to produce a `bytes32`.
pub fn evm_address_to_bytes32(addr: &[u8; 20]) -> [u8; 32] {
    address_word(addr)
}

/// Recover a 20-byte EVM address from a `bytes32`. Fails if the upper 12 bytes
/// are not all zero (i.e. it is not a valid EVM-origin address).
pub fn bytes32_to_evm_address(b: &[u8; 32]) -> Result<[u8; 20], AbiError> {
    if b[..12].iter().any(|&x| x != 0) {
        return Err(AbiError::InvalidEvmAddress);
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&b[12..]);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Unit tests for primitives. Golden-vector equality tests live in
// `tests/golden_vectors.rs` (integration test, so it can read the Foundry
// fixtures from `sra-stx/`).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_from_usize_is_big_endian() {
        let v = u256_from_usize(0xa4b1);
        // All zeros except last two bytes
        assert_eq!(&v[..30], &[0u8; 30]);
        assert_eq!(v[30], 0xa4);
        assert_eq!(v[31], 0xb1);
    }

    #[test]
    fn address_word_left_pads_with_zeros() {
        let addr = [0xab; 20];
        let w = address_word(&addr);
        assert_eq!(&w[..12], &[0u8; 12]);
        assert_eq!(&w[12..], &addr);
    }

    #[test]
    fn encode_bytes_dynamic_empty_produces_one_zero_word() {
        let out = encode_bytes_dynamic(b"");
        assert_eq!(out, [0u8; 32]);
    }

    #[test]
    fn encode_bytes_dynamic_pads_to_word_boundary() {
        let out = encode_bytes_dynamic(b"abc");
        assert_eq!(
            out.len(),
            64,
            "3 bytes -> length word (32) + padded data (32)"
        );
        assert_eq!(out[31], 3); // length
        assert_eq!(&out[32..35], b"abc");
        assert_eq!(&out[35..], &[0u8; 29]); // right-padding
    }

    #[test]
    fn encode_bytes_dynamic_no_padding_when_word_aligned() {
        let data = [0x11u8; 32];
        let out = encode_bytes_dynamic(&data);
        assert_eq!(out.len(), 64); // length + exactly 1 full word
        assert_eq!(out[31], 32);
        assert_eq!(&out[32..], &data);
    }

    #[test]
    fn encode_uint256_array_dynamic_empty() {
        let out = encode_uint256_array_dynamic(&[]);
        assert_eq!(out, [0u8; 32]);
    }

    #[test]
    fn encode_uint256_array_dynamic_appends_entries() {
        let entries = [[0x11u8; 32], [0x22u8; 32]];
        let out = encode_uint256_array_dynamic(&entries);
        assert_eq!(out.len(), 3 * 32);
        assert_eq!(out[31], 2);
        assert_eq!(&out[32..64], &[0x11u8; 32]);
        assert_eq!(&out[64..96], &[0x22u8; 32]);
    }

    /// Pinned against the `source_chain_mainnet` golden vector generated by
    /// `sra-stx/script/GoldenVectorGenerator.s.sol`. Catches any regression
    /// in `abi_encode_for_replay` / `replay_hash` without depending on the
    /// cross-repo fixture files.
    #[test]
    fn replay_hash_matches_solidity_for_source_chain_mainnet_vector() {
        // Reconstruct the `source_chain_mainnet` vector inputs.
        let mut account = [0u8; 32];
        account[30] = 0xab;
        account[31] = 0xcd;

        let mut amount_dep = [0u8; 32];
        amount_dep[28..].copy_from_slice(&14_000_000u32.to_be_bytes());
        let mut amount_tgt = [0u8; 32];
        amount_tgt[28..].copy_from_slice(&13_985_000u32.to_be_bytes());
        let mut target_chain = [0u8; 32];
        target_chain[30..].copy_from_slice(&42_161u16.to_be_bytes());

        let usdc_arb: [u8; 20] = [
            0xaf, 0x88, 0xd0, 0x65, 0xe7, 0x7c, 0x8c, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xb3,
            0xA4, 0x32, 0x26, 0x8e, 0x58, 0x31,
        ];

        let msg = Message {
            account,
            init_code: vec![],
            deposit_info: DepositInfo {
                deposit_param: TokenParam {
                    token: [0x44; 20],
                    amount: amount_dep,
                },
                target_chain_id: target_chain,
                target_param: TokenParam {
                    token: usdc_arb,
                    amount: amount_tgt,
                },
                extra: vec![],
            },
            action_idxs: vec![],
            app_id: [0u8; 32],
        };

        let mut source_chain_id = [0u8; 32];
        source_chain_id[31] = 1;

        // From golden-vectors/source_chain_mainnet.json `replayHash` field.
        let expected: [u8; 32] = [
            0xe7, 0x91, 0xc6, 0x02, 0x1a, 0x8f, 0xb3, 0xa5, 0x1e, 0x9f, 0xc1, 0x54, 0x66, 0xeb,
            0xfc, 0xb8, 0x77, 0xa2, 0x13, 0x81, 0xf6, 0xb3, 0xaa, 0xcc, 0x07, 0xd9, 0x67, 0x4c,
            0xae, 0xaf, 0xd0, 0x86,
        ];

        assert_eq!(replay_hash(&source_chain_id, &msg), expected);

        // Also sanity-check that the replay pre-image differs from the
        // top-level encoding (no adapter/handler prefix).
        let pre_image = abi_encode_for_replay(&source_chain_id, &msg);
        // First word = sourceChainId = 1 -> byte 31 is 0x01, rest zero.
        assert_eq!(&pre_image[..31], &[0u8; 31]);
        assert_eq!(pre_image[31], 1);
        // Second word = offset to Message = 0x40 (64).
        assert_eq!(&pre_image[32..63], &[0u8; 31]);
        assert_eq!(pre_image[63], 0x40);
    }
}
