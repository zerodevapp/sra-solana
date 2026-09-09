//! CCTP destination-leg test helpers.
//!
//! No mock program backs the destination flow — `verify_cctp_mint` only reads
//! accounts — so these helpers plant the two MessageTransmitter-v2-owned
//! accounts directly (`set_account` with the real transmitter id as owner)
//! and sign attestations with a dev secp256k1 key. Messages come from the
//! adapter's own `TestBurnMessage` encoder, pinned to the readers by the
//! adapter's golden-vector test.

use anchor_lang::{AccountDeserialize, AnchorSerialize, InstructionData, ToAccountMetas};
use libsecp256k1::{sign, Message, PublicKey, SecretKey};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction, keccak, pubkey::Pubkey, signature::Keypair, signer::Signer,
};
use solana_system_interface::program as system_program;
use sra_cctp_adapter::utils::cctp_state::{
    MessageTransmitterState, UsedNonce, MESSAGE_TRANSMITTER_DISCRIMINATOR, USED_NONCE_DISCRIMINATOR,
};
use sra_cctp_adapter::utils::message::TestBurnMessage;
use sra_common::{
    constants::{
        CCTP_MESSAGE_TRANSMITTER_V2_ID, CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        FINALITY_THRESHOLD_FINALIZED, SOLANA_CCTP_DOMAIN,
    },
    seeds::SEED_VERIFIED_FILL,
};

use super::*;

/// The live mainnet transmitter's message-format version — planted states and
/// sample messages carry it so header checks mirror production.
pub const CCTP_HEADER_VERSION: u32 = 1;
/// Arbitrum's CCTP domain: the sample route's bound source chain (42_161).
pub const ARBITRUM_CCTP_DOMAIN: u32 = 3;

pub fn derive_transmitter_state_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"message_transmitter"], &CCTP_MESSAGE_TRANSMITTER_V2_ID).0
}

pub fn derive_used_nonce_pda(nonce: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[b"used_nonce", nonce], &CCTP_MESSAGE_TRANSMITTER_V2_ID).0
}

pub fn derive_cctp_verified_fill_pda(nonce: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_VERIFIED_FILL, CCTP_ADAPTER_PROGRAM_ID.as_ref(), nonce],
        &CCTP_ADAPTER_PROGRAM_ID,
    )
}

/// The deterministic dev attester: a secp256k1 key plus its EVM address in
/// the `enabled_attesters` wire format.
pub fn test_attester() -> (SecretKey, Pubkey) {
    let secret = SecretKey::parse(&[0x42; 32]).expect("valid test secret key");
    let address = attester_address(&secret);
    (secret, address)
}

/// The EVM address of a secp256k1 key, exactly as the adapter recovers it:
/// last 20 bytes of `keccak(uncompressed pubkey)`, left-zero-padded to 32.
pub fn attester_address(secret: &SecretKey) -> Pubkey {
    let pubkey = PublicKey::from_secret_key(secret).serialize();
    let mut address = keccak::hash(&pubkey[1..]).0;
    address[..12].fill(0);
    Pubkey::new_from_array(address)
}

/// Concatenated 65-byte `r ‖ s ‖ v` signatures over `keccak(message)`, in the
/// key order given — callers wanting a valid multi-sig attestation must pass
/// keys pre-sorted by [`attester_address`].
pub fn sign_attestation(message: &[u8], keys: &[&SecretKey]) -> Vec<u8> {
    let hash = keccak::hash(message);
    let mut out = Vec::with_capacity(65 * keys.len());
    for secret in keys {
        let (signature, recovery_id) = sign(&Message::parse(&hash.0), secret);
        out.extend_from_slice(&signature.serialize());
        out.push(recovery_id.serialize() + 27);
    }
    out
}

fn plant_account(svm: &mut LiteSVM, address: Pubkey, owner: Pubkey, data: Vec<u8>) {
    svm.set_account(
        address,
        solana_sdk::account::Account {
            lamports: svm.minimum_balance_for_rent_exemption(data.len()),
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
}

/// Plant the transmitter state PDA (owner = the real transmitter program id;
/// nothing executes there — verification only reads it). Non-attester fields
/// mirror the live mainnet account.
pub fn plant_transmitter_state(
    svm: &mut LiteSVM,
    signature_threshold: u32,
    enabled_attesters: Vec<Pubkey>,
) {
    let state = MessageTransmitterState {
        owner: Pubkey::new_unique(),
        pending_owner: Pubkey::default(),
        attester_manager: Pubkey::new_unique(),
        pauser: Pubkey::new_unique(),
        paused: false,
        local_domain: SOLANA_CCTP_DOMAIN,
        version: CCTP_HEADER_VERSION,
        signature_threshold,
        enabled_attesters,
        max_message_body_size: 8192,
    };
    let mut data = MESSAGE_TRANSMITTER_DISCRIMINATOR.to_vec();
    state.serialize(&mut data).unwrap();
    plant_account(
        svm,
        derive_transmitter_state_pda(),
        CCTP_MESSAGE_TRANSMITTER_V2_ID,
        data,
    );
}

/// Plant the per-nonce mint receipt `receive_message` would have created.
pub fn plant_used_nonce(svm: &mut LiteSVM, nonce: [u8; 32], is_used: bool) {
    let mut data = USED_NONCE_DISCRIMINATOR.to_vec();
    UsedNonce { is_used }.serialize(&mut data).unwrap();
    plant_account(
        svm,
        derive_used_nonce_pda(&nonce),
        CCTP_MESSAGE_TRANSMITTER_V2_ID,
        data,
    );
}

/// A burn message bound to `route`/`mint` with live-mainnet-shaped defaults:
/// version-1 header and body, Arbitrum → Solana, recipient = the
/// TokenMessengerMinter, `mint_recipient` = the route vault ATA,
/// permissionless `destination_caller`. Tests mutate fields for reject cases.
pub fn sample_cctp_message(
    nonce: [u8; 32],
    route: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    max_fee: u64,
    fee_executed: u64,
) -> TestBurnMessage {
    TestBurnMessage {
        version: CCTP_HEADER_VERSION,
        source_domain: ARBITRUM_CCTP_DOMAIN,
        destination_domain: SOLANA_CCTP_DOMAIN,
        nonce,
        sender: evm_bytes32(0x51),
        recipient: CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        destination_caller: Pubkey::default(),
        min_finality_threshold: FINALITY_THRESHOLD_FINALIZED,
        finality_threshold_executed: FINALITY_THRESHOLD_FINALIZED,
        body_version: 1,
        burn_token: evm_bytes32(0x52),
        mint_recipient: derive_ata(route, mint),
        message_sender: evm_bytes32(0x53),
        amount,
        max_fee,
        fee_executed,
        expiration_block: 0,
        hook_data: vec![],
    }
}

pub fn derive_execution_lane(route: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"execution_lane", PROGRAM_ID.as_ref(), route.as_ref()],
        &CCTP_ADAPTER_PROGRAM_ID,
    )
    .0
}

/// Build a `verify_cctp_mint` instruction. The transmitter, used-nonce, and
/// verified-fill accounts are all derived from the real seeds, exactly as the
/// on-chain constraints require.
pub fn verify_cctp_mint_ix(
    payer: &Pubkey,
    route: &Pubkey,
    nonce: [u8; 32],
    message: Vec<u8>,
    attestation: Vec<u8>,
) -> Instruction {
    Instruction {
        program_id: CCTP_ADAPTER_PROGRAM_ID,
        accounts: sra_cctp_adapter::accounts::VerifyCctpMint {
            payer: *payer,
            message_transmitter: derive_transmitter_state_pda(),
            used_nonce: derive_used_nonce_pda(&nonce),
            route: *route,
            verified_fill: derive_cctp_verified_fill_pda(&nonce).0,
            system_program: system_program::ID,
            execution_lane: derive_execution_lane(route),
        }
        .to_account_metas(None),
        data: sra_cctp_adapter::instruction::VerifyCctpMint {
            nonce,
            message,
            attestation,
        }
        .data(),
    }
}

/// Create the sample route with the CCTP adapter (instead of Across) as its
/// one allowed adapter; returns the PDA and its `route_config_hash`.
pub fn create_cctp_route_with_hash(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Keypair,
    mint: Pubkey,
    salt: [u8; 32],
) -> (Pubkey, [u8; 32]) {
    let mut params = sample_create_route_params(mint, salt);
    params.allowed_adapters = vec![CCTP_ADAPTER_PROGRAM_ID];
    let hash = route_config_hash_for(&owner.pubkey(), &params);
    let (route_pda, _) = derive_route_pda(&owner.pubkey(), &hash, &params.salt);
    let ix = create_route_ix_with_hash(
        &payer.pubkey(),
        &owner.pubkey(),
        hash,
        params,
        &[CCTP_ADAPTER_PROGRAM_ID],
    );
    send_ix(svm, ix, payer, &[owner]).expect("create_route should succeed");
    (route_pda, hash)
}

pub fn fetch_cctp_verified_fill(
    svm: &LiteSVM,
    pda: &Pubkey,
) -> sra_cctp_adapter::state::VerifiedFill {
    let acct = svm.get_account(pda).expect("verified_fill account exists");
    sra_cctp_adapter::state::VerifiedFill::try_deserialize(&mut acct.data.as_slice())
        .expect("valid VerifiedFill")
}
