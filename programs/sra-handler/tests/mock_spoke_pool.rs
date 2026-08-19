//! Mock Across SpokePool destination-fill behaviors.
//!
//! `fill_relay` moves the output amount to the recipient and writes a `Filled`
//! `FillStatusAccount`; `mock_fill` plants arbitrary statuses for the adapter's
//! reject paths; `close_fill_pda` reclaims the PDA (the relayer-closure race the
//! adapter must survive). The `FillStatusAccount` layout is pinned against the
//! re-vendored Across IDL so the mock can't silently drift from the real program
//! the destination adapter reads on mainnet.

mod common;

use anchor_lang::{AccountDeserialize, Discriminator};
use common::*;
use litesvm::LiteSVM;
use mock_spoke_pool::{FillStatus, FillStatusAccount};
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use sra_across_adapter::utils::svm_spoke;
use sra_common::constants::ACROSS_SPOKE_POOL_ID;

const RELAY_HASH: [u8; 32] = [7u8; 32];
const RELAYER_STARTING_BALANCE: u64 = 5_000_000;
const OUTPUT_AMOUNT: u64 = 4_000_000;
const FILL_DEADLINE: u32 = 1_900_000_000;

/// `sha256("account:FillStatusAccount")[..8]` — the discriminator the deployed
/// Across program writes (and the re-vendored IDL declares).
const ACROSS_FILL_STATUS_DISCRIMINATOR: [u8; 8] = [105, 89, 88, 35, 24, 147, 178, 137];

struct Fill {
    relayer: Keypair,
    relayer_ata: Pubkey,
    recipient_ata: Pubkey,
    fill_status: Pubkey,
}

/// Stand up a mint + relayer/recipient ATAs and perform a realistic relayer fill.
fn fill_via_relay(svm: &mut LiteSVM, relay_hash: [u8; 32]) -> Fill {
    let relayer = funded_keypair(svm, 100 * SOL);
    let recipient = Pubkey::new_unique();

    let mint = create_mint(svm, &relayer, &relayer.pubkey(), 6);
    let relayer_ata = create_ata(svm, &relayer, &relayer.pubkey(), &mint);
    let recipient_ata = create_ata(svm, &relayer, &recipient, &mint);
    mint_to(
        svm,
        &relayer,
        &relayer,
        &mint,
        &relayer_ata,
        RELAYER_STARTING_BALANCE,
    );

    let ix = mock_fill_relay_ix(
        &relayer.pubkey(),
        &mint,
        &relayer_ata,
        &recipient_ata,
        relay_hash,
        OUTPUT_AMOUNT,
        FILL_DEADLINE,
    );
    send_ix(svm, ix, &relayer, &[]).expect("fill_relay should succeed");

    let (fill_status, _) = derive_fill_status_pda(&relay_hash);
    Fill {
        relayer,
        relayer_ata,
        recipient_ata,
        fill_status,
    }
}

#[test]
fn mock_spoke_pool_fill_relay_creates_filled_status_and_moves_tokens() {
    let mut svm = setup_svm();
    let fill = fill_via_relay(&mut svm, RELAY_HASH);

    assert_eq!(
        fetch_token_balance(&svm, &fill.recipient_ata),
        OUTPUT_AMOUNT
    );
    assert_eq!(
        fetch_token_balance(&svm, &fill.relayer_ata),
        RELAYER_STARTING_BALANCE - OUTPUT_AMOUNT
    );

    let acct = svm
        .get_account(&fill.fill_status)
        .expect("fill_status created");
    assert_eq!(acct.owner, ACROSS_SPOKE_POOL_ID);

    let fs = FillStatusAccount::try_deserialize(&mut acct.data.as_slice())
        .expect("valid FillStatusAccount");
    assert_eq!(fs.status, FillStatus::Filled);
    assert_eq!(fs.relayer, fill.relayer.pubkey());
    assert_eq!(fs.fill_deadline, FILL_DEADLINE);
}

#[test]
fn mock_spoke_pool_fill_status_layout_matches_revendored_idl() {
    // The mock's Anchor-derived discriminator, the IDL-generated type's
    // discriminator, and the deployed Across program's discriminator all agree.
    assert_eq!(
        FillStatusAccount::DISCRIMINATOR,
        ACROSS_FILL_STATUS_DISCRIMINATOR.as_slice()
    );
    assert_eq!(
        svm_spoke::accounts::FillStatusAccount::DISCRIMINATOR,
        ACROSS_FILL_STATUS_DISCRIMINATOR.as_slice()
    );

    let mut svm = setup_svm();
    let fill = fill_via_relay(&mut svm, RELAY_HASH);
    let data = svm
        .get_account(&fill.fill_status)
        .expect("fill_status created")
        .data;

    // 8 discriminator + 1 status (enum) + 32 relayer + 4 fill_deadline.
    // Load-bearing: this length pin is the only guard against a silently-added
    // trailing field — Borsh deserialization below ignores trailing bytes.
    assert_eq!(data.len(), 45);
    assert_eq!(&data[..8], ACROSS_FILL_STATUS_DISCRIMINATOR.as_slice());
    assert_eq!(data[8], 2, "FillStatus::Filled is variant 2");
    assert_eq!(&data[9..41], fill.relayer.pubkey().as_ref());
    assert_eq!(
        u32::from_le_bytes(data[41..45].try_into().unwrap()),
        FILL_DEADLINE
    );

    // The exact bytes the mock wrote round-trip through the IDL-generated type.
    let generated = svm_spoke::accounts::FillStatusAccount::try_deserialize(&mut data.as_slice())
        .expect("mock bytes deserialize as the re-vendored FillStatusAccount");
    assert!(matches!(
        generated.status,
        svm_spoke::types::FillStatus::Filled
    ));
    assert_eq!(generated.relayer, fill.relayer.pubkey());
    assert_eq!(generated.fill_deadline, FILL_DEADLINE);
}

#[test]
fn mock_spoke_pool_mock_fill_plants_unfilled_status() {
    let mut svm = setup_svm();
    let relayer = funded_keypair(&mut svm, SOL);

    send_ix(
        &mut svm,
        mock_fill_ix(&relayer.pubkey(), RELAY_HASH, FillStatus::Unfilled),
        &relayer,
        &[],
    )
    .expect("mock_fill should succeed");

    let (pda, _) = derive_fill_status_pda(&RELAY_HASH);
    let acct = svm.get_account(&pda).expect("fill_status created");
    assert_eq!(acct.owner, ACROSS_SPOKE_POOL_ID);

    let fs = FillStatusAccount::try_deserialize(&mut acct.data.as_slice())
        .expect("valid FillStatusAccount");
    assert_eq!(fs.status, FillStatus::Unfilled);
    // Load-bearing: `Unfilled` is variant 0, so a no-op `mock_fill` would also
    // leave a zero-init status — the relayer pin proves the instruction wrote.
    assert_eq!(fs.relayer, relayer.pubkey());
}

#[test]
fn mock_spoke_pool_close_fill_pda_reclaims_account() {
    let mut svm = setup_svm();
    let relayer = funded_keypair(&mut svm, SOL);

    send_ix(
        &mut svm,
        mock_fill_ix(&relayer.pubkey(), RELAY_HASH, FillStatus::Filled),
        &relayer,
        &[],
    )
    .expect("mock_fill should succeed");

    let (pda, _) = derive_fill_status_pda(&RELAY_HASH);
    assert!(
        svm.get_account(&pda)
            .map(|a| a.lamports > 0)
            .unwrap_or(false),
        "fill_status exists before close"
    );

    let timestamp = now(&svm).max(1);
    set_time(&mut svm, timestamp);

    send_ix(
        &mut svm,
        close_fill_pda_ix(&relayer.pubkey(), &pda),
        &relayer,
        &[],
    )
    .expect("close_fill_pda should succeed");

    let after = svm.get_account(&pda);
    assert!(
        after.map(|a| a.lamports == 0).unwrap_or(true),
        "fill_status reclaimed after close"
    );
}

#[test]
fn mock_close_requires_recorded_relayer_and_passed_deadline() {
    let mut svm = setup_svm();
    let payer = funded_keypair(&mut svm, 100 * SOL);
    let relayer = funded_keypair(&mut svm, SOL);
    let stranger = funded_keypair(&mut svm, SOL);
    let mint = create_mint(&mut svm, &payer, &payer.pubkey(), 6);
    let relayer_ata = create_ata(&mut svm, &payer, &relayer.pubkey(), &mint);
    let recipient = Keypair::new();
    let recipient_ata = create_ata(&mut svm, &payer, &recipient.pubkey(), &mint);
    mint_to(&mut svm, &payer, &payer, &mint, &relayer_ata, 1);
    let relay_hash = [0x91; 32];
    let deadline = u32::try_from(now(&svm) + 10).unwrap();
    send_ix(
        &mut svm,
        mock_fill_relay_ix(
            &relayer.pubkey(),
            &mint,
            &relayer_ata,
            &recipient_ata,
            relay_hash,
            1,
            deadline,
        ),
        &relayer,
        &[],
    )
    .unwrap();
    let status = derive_fill_status_pda(&relay_hash).0;

    send_ix(
        &mut svm,
        close_fill_pda_ix(&stranger.pubkey(), &status),
        &stranger,
        &[],
    )
    .expect_err("only recorded relayer may close");
    send_ix(
        &mut svm,
        close_fill_pda_ix(&relayer.pubkey(), &status),
        &relayer,
        &[],
    )
    .expect_err("deadline must strictly pass");

    set_time(&mut svm, i64::from(deadline) + 1);
    send_ix(
        &mut svm,
        close_fill_pda_ix(&relayer.pubkey(), &status),
        &relayer,
        &[],
    )
    .expect("recorded relayer closes after deadline");
}
