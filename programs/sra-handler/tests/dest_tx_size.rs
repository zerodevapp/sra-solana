mod common;

use common::*;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{
    v1::{MessageError, MAX_TRANSACTION_SIZE},
    VersionedMessage,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use sra_common::{
    constants::{MAX_ACTION_IDXS, MAX_TEMPLATE_SLOTS},
    quote::{action_context_hash, signed_quote_message},
    types::{AccountTemplate, FixedRole, MinOut, SlotRole, SolanaAction},
};

const AMOUNT: u64 = 1_000_000;
const FILL_ID: [u8; 32] = [9u8; 32];
const SOURCE_CHAIN_ID: u64 = 42_161;
const BPS_TOTAL: u16 = 10_000;
const EXPIRY: i64 = 1_900_000_000;
const VAULT: u64 = 4_000_000;
const MIN_OUT: u64 = 2_900_000;
const INNER_DATA_LEN: usize = 256;

struct Dest {
    payer: Keypair,
    owner: Keypair,
    route: Pubkey,
    route_config_hash: [u8; 32],
    mint: Pubkey,
}

fn dest() -> Dest {
    let payer = Keypair::new();
    let owner = Keypair::new();
    let mint = Pubkey::new_unique();
    let params = sample_create_route_params(mint, [7u8; 32]);
    let route_config_hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route, _) = derive_route_pda(&owner.pubkey(), &route_config_hash, &params.salt);
    Dest {
        payer,
        owner,
        route,
        route_config_hash,
        mint,
    }
}

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

fn fallback_finalize_ixs(d: &Dest) -> Vec<Instruction> {
    let relay_data = sample_relay_data(d.route, d.mint, SOURCE_CHAIN_ID, AMOUNT);
    let relay_hash = relay_hash_for(&relay_data);
    let verified_fill = derive_verified_fill_pda(&relay_hash).0;
    vec![
        verify_across_fill_after_close_ix(
            &d.payer.pubkey(),
            &d.owner.pubkey(),
            &d.route,
            relay_hash,
            relay_data,
        ),
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

fn finalize_only_ix(d: &Dest) -> Instruction {
    verify_finalize_ixs(d).remove(1)
}

fn max_spl_execute_ix(d: &Dest) -> Instruction {
    let recipients: Vec<Pubkey> = (0..MAX_ACTION_IDXS).map(|_| Pubkey::new_unique()).collect();
    let atas: Vec<Pubkey> = recipients.iter().map(|r| derive_ata(r, &d.mint)).collect();
    execute_actions_ix(&d.payer.pubkey(), &d.route, FILL_ID, &d.mint, &atas)
}

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

fn verified_cpi_ixs(d: &Dest, n: u8) -> Vec<Instruction> {
    let mut ixs = Vec::new();
    let mut action_config = Vec::new();
    let mut runtimes = Vec::new();
    let mut metas = Vec::new();

    for i in 0..n {
        let output_mint = Pubkey::new_unique();
        let quoter = Keypair::new();
        action_config.push(SolanaAction::VerifiedCpi {
            program_id: mock_swap::ID,
            input_mint: d.mint,
            input_bps: BPS_TOTAL / n as u16,
            account_template: max_template(),
            output_mint,
            min_out: MinOut::SignedQuote {
                quoter: quoter.pubkey(),
            },
            allow_permissioned_output: false,
        });
        metas.push(AccountMeta::new_readonly(mock_swap::ID, false));
        for _ in 0..MAX_TEMPLATE_SLOTS {
            metas.push(AccountMeta::new(Pubkey::new_unique(), false));
        }
        runtimes.push(runtime(vec![0u8; INNER_DATA_LEN], MIN_OUT, EXPIRY));

        let context_hash = action_context_hash(&d.route, &FILL_ID, i);
        let message =
            signed_quote_message(&context_hash, &d.mint, &output_mint, VAULT, MIN_OUT, EXPIRY);
        ixs.push(ed25519_quote_ix(&quoter, &message));
    }

    ixs.push(execute_actions_ix_full(
        &d.payer.pubkey(),
        &d.route,
        FILL_ID,
        runtimes,
        &d.mint,
        metas,
    ));
    ixs
}

fn build_dest_v1_transaction(
    d: &Dest,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> VersionedTransaction {
    build_v1_transaction(&d.payer, signers, ixs, Hash::default(), DEFAULT_V1_CONFIG)
        .expect("build destination v1 transaction")
}

fn assert_v1_shape(
    label: &str,
    transaction: &VersionedTransaction,
    expected_size: usize,
    expected_accounts: usize,
) {
    let size = serialized_v1_len(transaction);
    let accounts = transaction.message.static_account_keys().len();
    println!("{label}: {size} bytes, {accounts} accounts");
    assert!(matches!(&transaction.message, VersionedMessage::V1(_)));
    assert_eq!(size, expected_size);
    assert_eq!(accounts, expected_accounts);
    assert!(size <= MAX_TRANSACTION_SIZE);
}

#[test]
fn dest_verify_finalize_v1_size() {
    let d = dest();
    let transaction = build_dest_v1_transaction(&d, &verify_finalize_ixs(&d), &[]);

    assert_v1_shape("dest verify+finalize v1", &transaction, 954, 14);
}

#[test]
fn dest_owner_fallback_finalize_v1_size() {
    let d = dest();
    let ixs = fallback_finalize_ixs(&d);
    let transaction = build_dest_v1_transaction(&d, &ixs, &[&d.owner]);

    assert_v1_shape("dest owner fallback+finalize v1", &transaction, 1_051, 15);
    assert_eq!(transaction.message.header().num_required_signatures, 2);
}

#[test]
fn dest_finalize_plus_max_spl_v1_size() {
    let d = dest();
    let ixs = vec![finalize_only_ix(&d), max_spl_execute_ix(&d)];
    let transaction = build_dest_v1_transaction(&d, &ixs, &[]);

    assert_v1_shape("dest finalize+max SPL transfers v1", &transaction, 804, 17);
}

#[test]
fn dest_single_verified_cpi_v1_size() {
    let d = dest();
    let transaction = build_dest_v1_transaction(&d, &verified_cpi_ixs(&d, 1), &[]);

    assert_v1_shape("dest single VerifiedCpi v1", &transaction, 2_099, 43);
}

#[test]
fn dest_finalize_plus_single_verified_cpi_v1_size() {
    let d = dest();
    let mut ixs = verified_cpi_ixs(&d, 1);
    ixs.insert(1, finalize_only_ix(&d));
    let transaction = build_dest_v1_transaction(&d, &ixs, &[]);

    assert_v1_shape(
        "dest finalize+single VerifiedCpi v1",
        &transaction,
        2_313,
        47,
    );
}

#[test]
fn dest_max_verified_cpi_v1_exceeds_limits() {
    let d = dest();
    let ixs = verified_cpi_ixs(&d, MAX_ACTION_IDXS as u8);
    let message = compile_v1_message(&d.payer.pubkey(), &ixs, Hash::default(), DEFAULT_V1_CONFIG)
        .expect("compile maximum VerifiedCpi v1 message");
    let accounts = message.account_keys.len();
    let transaction =
        VersionedTransaction::try_new(VersionedMessage::V1(message.clone()), &[&d.payer])
            .expect("sign maximum VerifiedCpi v1 transaction");
    let size = serialized_v1_len(&transaction);

    println!("dest four VerifiedCpi v1: {size} bytes, {accounts} accounts");
    assert_eq!(size, 6_830);
    assert_eq!(accounts, 139);
    assert_eq!(message.validate(), Err(MessageError::TooManyAddresses));
    assert!(size > MAX_TRANSACTION_SIZE);
}

#[test]
fn dest_cctp_verify_finalize_v1_size() {
    use common::cctp::*;

    let d = dest();
    let nonce = FILL_ID;
    let message = sample_cctp_message(nonce, &d.route, &d.mint, AMOUNT, 4_000, 0).encode();
    let attestation = vec![0u8; 2 * 65];
    let (verified_fill, _) = derive_cctp_verified_fill_pda(&nonce);
    let ixs = vec![
        verify_cctp_mint_ix(&d.payer.pubkey(), &d.route, nonce, message, attestation),
        finalize_fill_ix(
            &d.payer.pubkey(),
            &d.route,
            d.route_config_hash,
            &verified_fill,
            nonce,
            &d.mint,
        ),
    ];
    let transaction = build_dest_v1_transaction(&d, &ixs, &[]);

    assert_v1_shape("dest CCTP verify+finalize v1", &transaction, 1_212, 14);
}
