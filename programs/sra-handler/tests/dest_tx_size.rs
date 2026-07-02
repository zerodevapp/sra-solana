//! Destination packet-size validation.
//!
//! Solana enforces a hard ~1232-byte packet limit on a serialized transaction.
//! The destination settlement flow is composed from three instructions, batched
//! by the SDK into versioned transactions:
//!   * `verify_across_fill` (adapter) + `finalize_fill` (handler) — records the
//!     fill and moves the bridged amount into the route's execution vault.
//!   * `execute_actions` (handler) — runs the route's committed `action_config`
//!     (one or more `SplTransfer`s, and/or a `VerifiedCpi` swap/deposit).
//!
//! These canaries pin each shape's serialized size so a future change that grows
//! the static account set (or the action payload) is caught before it silently
//! pushes a real transaction past the packet limit.
//!
//! A transaction's serialized size is a pure function of its account count, its
//! signature count, and its instruction-data length — not of whether the
//! referenced accounts exist on-chain. The route stores its `action_config`
//! on-chain (set once at `create_route`); `execute_actions` reads it from the
//! route account and carries only the per-fill runtime data (e.g. a
//! `VerifiedCpi`'s inner instruction bytes and `min_amount_out`) plus a single
//! `quoter` account that co-signs the whole batch, attesting every action's
//! `min_amount_out` and the shared `quote_expiry` — one signature replaces what
//! used to be a per-action ed25519 quote instruction. So these tests build the
//! genuine instructions, via the same builders the SDK uses, at the heaviest
//! payload the program accepts, sign them with every required signer (including
//! the co-signing quoter, whose signature and account key are NOT compressible
//! by an address lookup table), and serialize them — they never submit.
//! (Executing the `VerifiedCpi` path end-to-end belongs to the compute-budget and
//! end-to-end suites; here only the wire size matters.)
//!
//! Two findings the shapes make concrete:
//!   * `verify_across_fill` + `finalize_fill` is **action-config independent** —
//!     neither instruction carries `action_config`. There is one verify+finalize
//!     shape regardless of how many actions the route commits; it is the floor
//!     canary, and it fits even without an address lookup table.
//!   * An address lookup table is **mandatory only for the heavy
//!     `execute_actions(VerifiedCpi)` shape** — a Jupiter-scale inner route fills
//!     the account list, and the co-signing quoter's signature and key still ride
//!     outside the table. The lighter shapes fit unaided.

mod common;

use common::*;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    packet::PACKET_DATA_SIZE,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};
use sra_common::{
    constants::{MAX_ACTION_IDXS, MAX_TEMPLATE_SLOTS},
    types::{AccountTemplate, FixedRole, SlotRole, SolanaAction},
};

const AMOUNT: u64 = 1_000_000;
const FILL_ID: [u8; 32] = [9u8; 32];
/// Matches `sample_create_route_params` — the route's bound source chain.
const SOURCE_CHAIN_ID: u64 = 42_161;
const BPS_TOTAL: u16 = 10_000;
/// Far-future fixed unix expiry for the representative quote batch (never executed).
const EXPIRY: i64 = 1_900_000_000;
// Representative VerifiedCpi quote figure (sized, never executed here).
const MIN_OUT: u64 = 2_900_000;
/// Representative relayer-supplied inner-instruction length for a `VerifiedCpi`
/// (e.g. an aggregator route plan). It is NOT bounded by `validate_action_config`
/// — it rides as runtime instruction data — so a real-world length is the honest
/// size driver, not the few-byte mock-swap payload.
const INNER_DATA_LEN: usize = 256;

struct Dest {
    payer: Keypair,
    route: Pubkey,
    route_config_hash: [u8; 32],
    mint: Pubkey,
}

/// A signer plus a real route PDA + its `route_config_hash`, both derived (not
/// submitted): every instruction PDA hangs off `route` + `mint`, and the size of
/// the resulting transaction is independent of the route's on-chain existence.
fn dest() -> Dest {
    let owner = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let params = sample_create_route_params(mint, [7u8; 32]);
    let route_config_hash = route_config_hash_for(&owner, &params);
    let (route, _) = derive_route_pda(&owner, &route_config_hash, &params.salt);
    Dest {
        payer: Keypair::new(),
        route,
        route_config_hash,
        mint,
    }
}

/// Compile `ixs` into a v0 transaction (optionally with lookup tables) and sign it
/// with `payer` plus any `extra_signers` (e.g. a co-signing quoter) — every
/// destination shape's fee payer plus whatever else the batch requires signatures
/// from.
fn compile_v0(
    payer: &Keypair,
    ixs: &[Instruction],
    lookup_tables: &[AddressLookupTableAccount],
    extra_signers: &[&Keypair],
) -> VersionedTransaction {
    let msg = v0::Message::try_compile(&payer.pubkey(), ixs, lookup_tables, Hash::default())
        .expect("compile v0 message");
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);
    VersionedTransaction::try_new(VersionedMessage::V0(msg), &signers).expect("sign versioned tx")
}

/// Like `compile_v0` but signed by `payer` plus `extra` co-signers — used when the
/// instruction set requires a co-signing quoter (its signature adds 64 B and its
/// account key adds 32 B; signers cannot ride in an address lookup table, so this
/// MUST be used for any measured transaction that requires a co-signer).
fn compile_v0_signed(
    ixs: &[Instruction],
    payer: &Keypair,
    extra: &[&Keypair],
    alts: &[AddressLookupTableAccount],
) -> VersionedTransaction {
    let msg = v0::Message::try_compile(&payer.pubkey(), ixs, alts, Hash::default())
        .expect("compile v0 message");
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    VersionedTransaction::try_new(VersionedMessage::V0(msg), &signers).expect("sign versioned tx")
}

fn serialized_len(tx: &VersionedTransaction) -> usize {
    bincode::serialize(tx).expect("bincode serialize tx").len()
}

// --- Shape builders ------------------------------------------------------------

/// The `verify_across_fill` + `finalize_fill` instruction pair for a fresh fill.
fn verify_finalize_ixs(d: &Dest) -> Vec<Instruction> {
    let relay_data = sample_relay_data(d.route, d.mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let (verified_fill, _) = derive_verified_fill_pda(&relay_hash);
    vec![
        verify_across_fill_ix(&d.payer.pubkey(), &d.route, relay_hash, relay_data),
        finalize_fill_ix(
            &d.payer.pubkey(),
            &d.route,
            d.route_config_hash,
            &verified_fill,
            relay_hash,
            &d.mint,
        ),
    ]
}

/// `finalize_fill` alone (the second half of the pair), for batching with
/// `execute_actions`.
fn finalize_only_ix(d: &Dest) -> Instruction {
    verify_finalize_ixs(d).remove(1)
}

/// `execute_actions` carrying the maximum number of `SplTransfer`s (the heaviest
/// static-account action shape), one recipient ATA per action.
fn max_spl_execute_ix(d: &Dest) -> Instruction {
    let recipients: Vec<Pubkey> = (0..MAX_ACTION_IDXS).map(|_| Pubkey::new_unique()).collect();
    let atas: Vec<Pubkey> = recipients.iter().map(|r| derive_ata(r, &d.mint)).collect();
    execute_actions_ix(&d.payer.pubkey(), &d.route, FILL_ID, &d.mint, &atas)
}

/// A template pinning every handler-controlled role, padded with `Free` routing
/// slots to `MAX_TEMPLATE_SLOTS` — the account-count envelope of a Jupiter-scale
/// inner route (the concrete aggregator template is a later addition; the slot
/// count is the cap the program enforces today).
fn max_template() -> AccountTemplate {
    let mut slots = vec![
        SlotRole::Fixed(FixedRole::TokenProgram),
        SlotRole::Fixed(FixedRole::SigningAuthority),
        SlotRole::Fixed(FixedRole::Source),
        SlotRole::Fixed(FixedRole::Destination),
        SlotRole::Fixed(FixedRole::OutputMint),
    ];
    while slots.len() < MAX_TEMPLATE_SLOTS {
        slots.push(SlotRole::Free);
    }
    AccountTemplate { slots }
}

/// `n` max-template `VerifiedCpi` actions sharing one co-signing quoter, plus the
/// single `execute_actions` that runs them atomically. Each action carries a
/// representative relayer-supplied `inner_data`; the quoter's co-signature attests
/// every action's `min_amount_out` and the batch `quote_expiry`. Returns
/// `(vec![execute_actions], quoter)`, the exact shape the SDK submits for a route
/// of `n` verified swaps — the caller signs with `payer` + the returned quoter.
fn verified_cpi_ixs(d: &Dest, n: u8) -> (Vec<Instruction>, Keypair) {
    let quoter = Keypair::new();
    let mut action_config = Vec::new();
    let mut runtimes = Vec::new();
    let mut metas = Vec::new();

    for _ in 0..n {
        let output_mint = Pubkey::new_unique();
        action_config.push(SolanaAction::VerifiedCpi {
            program_id: mock_swap::ID,
            input_mint: d.mint,
            input_bps: BPS_TOTAL / n as u16,
            account_template: max_template(),
            output_mint,
            quoter: quoter.pubkey(),
            allow_permissioned_output: false,
        });
        // The per-action run: [inner program, then one account per template slot].
        metas.push(AccountMeta::new_readonly(mock_swap::ID, false));
        for _ in 0..MAX_TEMPLATE_SLOTS {
            metas.push(AccountMeta::new(Pubkey::new_unique(), false));
        }
        runtimes.push(runtime(vec![0u8; INNER_DATA_LEN], MIN_OUT));
    }

    let exec = execute_actions_ix_full(
        &d.payer.pubkey(),
        &d.route,
        FILL_ID,
        runtimes,
        EXPIRY,
        Some(quoter.pubkey()),
        &d.mint,
        metas,
    );
    (vec![exec], quoter)
}

// --- verify_across_fill + finalize_fill (action-config-independent floor) ------

#[test]
fn dest_verify_finalize_2ix_fits_within_packet_limit_with_alt() {
    let d = dest();
    let ixs = verify_finalize_ixs(&d);
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[alt], &[]));

    println!("dest verify+finalize 2-IX with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "verify+finalize with ALT is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

#[test]
fn dest_verify_finalize_2ix_fits_without_alt() {
    let d = dest();
    let ixs = verify_finalize_ixs(&d);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[], &[]));

    // Unlike the source pair, verify+finalize touches few accounts and fits
    // unaided — an address lookup table is not mandatory for this shape.
    println!("dest verify+finalize 2-IX without ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "verify+finalize without ALT is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

// --- execute_actions: SplTransfer (max actions) --------------------------------

#[test]
fn dest_finalize_plus_max_spl_execute_actions_fits_with_alt() {
    let d = dest();
    let ixs = vec![finalize_only_ix(&d), max_spl_execute_ix(&d)];
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[alt], &[]));

    println!("dest finalize + execute_actions(max SplTransfer) with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "finalize + max-SplTransfer execute_actions with ALT is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

// --- execute_actions: single VerifiedCpi (Jupiter-scale, ALT-mandatory) --------

#[test]
fn dest_single_verified_cpi_execute_actions_fits_with_alt() {
    let d = dest();
    let (ixs, quoter) = verified_cpi_ixs(&d, 1);
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[alt], &[&quoter]));

    println!("dest execute_actions(1 VerifiedCpi, max template) with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "single-VerifiedCpi execute_actions with ALT is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit",
    );
}

#[test]
fn dest_single_verified_cpi_execute_actions_exceeds_without_alt() {
    let d = dest();
    let (ixs, quoter) = verified_cpi_ixs(&d, 1);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[], &[&quoter]));

    // The Jupiter-scale account list pushes even a single verified swap past the
    // limit unaided — an address lookup table is mandatory for it.
    println!("dest execute_actions(1 VerifiedCpi, max template) without ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size > PACKET_DATA_SIZE,
        "single-VerifiedCpi execute_actions without ALT is {size} bytes, expected to exceed the {PACKET_DATA_SIZE}-byte packet limit (ALT-mandatory shape)",
    );
}

#[test]
fn dest_finalize_plus_single_verified_cpi_execute_actions_fits_with_alt() {
    let d = dest();
    // [finalize, execute_actions(1 VerifiedCpi)] — the SDK can batch the settle and
    // one verified swap; measure whether the combined shape still fits.
    let (mut ixs, quoter) = verified_cpi_ixs(&d, 1);
    ixs.insert(0, finalize_only_ix(&d));
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[alt], &[&quoter]));

    println!("dest finalize + execute_actions(1 VerifiedCpi, max template) with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= PACKET_DATA_SIZE,
        "finalize + 1-VerifiedCpi execute_actions with ALT is {size} bytes, exceeds {PACKET_DATA_SIZE}-byte packet limit — split into separate transactions (FillReceipt gates the sequence)",
    );
}

// Measured 1035 B. The ceiling is deliberately tighter than PACKET_DATA_SIZE
// (1232) so a regression eroding the two-action headroom fails here before it
// reaches the wire limit.
const TWO_VERIFIED_CPI_CEILING: usize = 1_100;

#[test]
fn dest_two_verified_cpi_execute_actions_fits_with_alt() {
    let d = dest();
    let (ixs, quoter) = verified_cpi_ixs(&d, 2);
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0_signed(&ixs, &d.payer, &[&quoter], &[alt]));

    println!("dest execute_actions(2 VerifiedCpi, max template) with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size <= TWO_VERIFIED_CPI_CEILING,
        "two VerifiedCpi actions must fit in one transaction: {size} > {TWO_VERIFIED_CPI_CEILING}"
    );
}

// --- execute_actions: the program-accepted maximum action set ------------------
//
// `validate_action_config` accepts up to `MAX_ACTION_IDXS` actions of any mix, with
// no cap on the number of `VerifiedCpi` actions, and `execute_actions` runs the
// WHOLE committed list atomically in one instruction (no chunking). Each extra
// `VerifiedCpi` adds an address-lookup-table-INCOMPRESSIBLE cost — its own committed
// action bytes and runtime `inner_data` — so the genuine worst case is
// `MAX_ACTION_IDXS` verified swaps sharing one co-signing quoter. This canary sizes
// that ceiling.

#[test]
fn dest_max_verified_cpi_execute_actions_exceeds_packet_limit_even_with_alt() {
    let d = dest();
    let (ixs, quoter) = verified_cpi_ixs(&d, MAX_ACTION_IDXS as u8);
    let alt = representative_lookup_table(&ixs);
    let size = serialized_len(&compile_v0(&d.payer, &ixs, &[alt], &[&quoter]));

    // FINDING: the program permits routes whose single, atomic `execute_actions`
    // cannot fit in one transaction even with an address lookup table — an ALT
    // compresses account addresses, but the per-action committed action bytes and
    // runtime data are incompressible. Because actions execute atomically
    // (no partial execution), such a route's actions can never run on-chain; the
    // bridged funds settle into the execution vault and are owner-recoverable, but
    // the intended actions do not execute. The route-creation / SDK layer must
    // bound a route's action + account + data budget so its `execute_actions` fits.
    println!("dest execute_actions({MAX_ACTION_IDXS} VerifiedCpi, max template) with ALT: {size} / {PACKET_DATA_SIZE} bytes");
    assert!(
        size > PACKET_DATA_SIZE,
        "max-action VerifiedCpi execute_actions with ALT is {size} bytes; if this now fits, the heaviest-route assumption changed — re-derive the per-route action/account/data budget",
    );
}
