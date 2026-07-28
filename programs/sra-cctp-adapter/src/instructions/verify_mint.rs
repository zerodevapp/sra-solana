//! Destination mint verification.
//!
//! Records a `VerifiedFill` once this adapter has independently confirmed a
//! CCTP v2 mint. The Across pattern does not transfer: CCTP's `used_nonce`
//! PDA is seeded only by the Iris-assigned nonce and stores only `is_used`,
//! so it commits to nothing about the message contents. Verification instead
//! combines two independent proofs:
//!
//! 1. **Attestation re-verification binds the contents** — Circle's exact
//!    signature check re-runs over `keccak256(message)` against the live
//!    attester set read from the pinned MessageTransmitter state PDA, the
//!    same trust root the transmitter used when it minted.
//! 2. **Used-nonce existence proves the mint executed** — the pinned
//!    `["used_nonce", nonce]` receipt must exist and be `is_used`; without
//!    it, an attested-but-never-received message could record evidence
//!    before any funds arrived.
//!
//! The (now attestation-bound) header and burn body are then cross-checked
//! against the committed route. `destination_caller` is deliberately not
//! checked — `receive_message` is permissionless by design, and verification
//! must not depend on who submitted the mint.
//!
//! The resulting `VerifiedFill` is the trust boundary the handler reads
//! (across the program boundary) to authorize fund movement; a vault-balance
//! increase alone never proves a legitimate fill.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address;
use solana_program::keccak;
use sra_common::{
    constants::{
        chain_id_for_cctp_domain, CCTP_MESSAGE_TRANSMITTER_V2_ID, CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
    },
    seeds::SEED_VERIFIED_FILL,
    types::SolanaSraRoute,
};

use crate::events::CctpMintVerified;
use crate::state::VerifiedFill;
use crate::utils::attestation::verify_attestation_signatures;
use crate::utils::cctp_state::{deserialize_message_transmitter, deserialize_used_nonce};
use crate::utils::message::{BurnMessage, CctpMessage, BURN_MESSAGE_BODY_VERSION};
use crate::CctpAdapterError;

#[derive(Accounts)]
#[instruction(nonce: [u8; 32])]
pub struct VerifyCctpMint<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: MessageTransmitter v2 state, pinned by seeds + owner; read as
    /// raw bytes (never CPI'd), so its layout is validated by
    /// `deserialize_message_transmitter` in the handler body.
    #[account(
        seeds = [b"message_transmitter"],
        bump,
        seeds::program = CCTP_MESSAGE_TRANSMITTER_V2_ID,
        owner = CCTP_MESSAGE_TRANSMITTER_V2_ID,
    )]
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: per-nonce receipt PDA created by `receive_message`, pinned by
    /// seeds + owner; proves the mint executed. Parsed exact-layout in-body.
    #[account(
        seeds = [b"used_nonce", nonce.as_ref()],
        bump,
        seeds::program = CCTP_MESSAGE_TRANSMITTER_V2_ID,
        owner = CCTP_MESSAGE_TRANSMITTER_V2_ID,
    )]
    pub used_nonce: UncheckedAccount<'info>,
    /// The committed destination route the mint is being verified against. An
    /// `Account` here requires it to be a genuine handler-owned route.
    pub route: Box<Account<'info, SolanaSraRoute>>,
    #[account(
        init,
        payer = payer,
        space = 8 + VerifiedFill::INIT_SPACE,
        seeds = [SEED_VERIFIED_FILL, crate::ID.as_ref(), nonce.as_ref()],
        bump,
    )]
    pub verified_fill: Box<Account<'info, VerifiedFill>>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<VerifyCctpMint>,
    nonce: [u8; 32],
    message: Vec<u8>,
    attestation: Vec<u8>,
) -> Result<()> {
    // 1. The live transmitter configuration (the deserializer enforces its
    //    internal validity).
    let state = {
        let data = ctx.accounts.message_transmitter.try_borrow_data()?;
        deserialize_message_transmitter(&data)?
    };

    // 2. Circle's attestation binds the full caller-supplied message bytes to
    //    the same trust root the transmitter minted under.
    verify_attestation_signatures(
        &keccak::hash(&message),
        &attestation,
        state.signature_threshold,
        &state.enabled_attesters,
    )?;

    // 3. The used-nonce receipt proves `receive_message` executed for this
    //    nonce — an attested-but-never-received message records no evidence.
    let used_nonce = {
        let data = ctx.accounts.used_nonce.try_borrow_data()?;
        deserialize_used_nonce(&data)?
    };
    require!(used_nonce.is_used, CctpAdapterError::NonceNotUsed);

    // 4. Header cross-checks over the attestation-bound bytes. The nonce
    //    equality binds the seed argument (which pinned `used_nonce` and seeds
    //    `verified_fill`) to the message; the recipient check requires the
    //    used nonce to belong to a *burn* message, not some other receiver's.
    let msg = CctpMessage::parse(&message)?;
    require!(
        msg.version() == state.version,
        CctpAdapterError::MalformedMessage
    );
    require!(
        msg.destination_domain() == state.local_domain,
        CctpAdapterError::DestinationDomainMismatch
    );
    require!(msg.nonce() == nonce, CctpAdapterError::NonceMismatch);
    require_keys_eq!(
        msg.recipient(),
        CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        CctpAdapterError::InvalidRecipientProgram
    );
    // `destination_caller` deliberately unchecked: receive is permissionless.

    // 5. Body cross-checks against the committed route. The ATA derivation
    //    binds the mint, so delivered funds are necessarily `expected_mint`;
    //    the fee bounds mirror the CCTP program's own (`fee_executed ≤
    //    max_fee < amount`).
    let body = BurnMessage::parse(msg.body())?;
    require!(
        body.version() == BURN_MESSAGE_BODY_VERSION,
        CctpAdapterError::MalformedMessage
    );
    let route_key = ctx.accounts.route.key();
    let expected_mint = ctx.accounts.route.expected_mint;
    let source_chain_id = ctx.accounts.route.source_chain_id;
    require_keys_eq!(
        body.mint_recipient(),
        get_associated_token_address(&route_key, &expected_mint),
        CctpAdapterError::MintRecipientMismatch
    );
    require!(
        chain_id_for_cctp_domain(msg.source_domain()) == Some(source_chain_id),
        CctpAdapterError::SourceDomainMismatch
    );
    let amount = body.amount()?;
    let max_fee = body.max_fee()?;
    let fee_executed = body.fee_executed()?;
    require!(
        fee_executed <= max_fee && max_fee < amount,
        CctpAdapterError::FeeExceedsMaxFee
    );

    // 6. Record the verification artifact. Both subtractions are safe under
    //    the fee bounds above, and `delivered ≥ committed_minimum` (the
    //    handler's `finalize_fill` invariant) holds because
    //    `fee_executed ≤ max_fee`.
    let delivered = amount - fee_executed;
    let committed_minimum = amount - max_fee;
    ctx.accounts.verified_fill.set_inner(VerifiedFill {
        adapter: crate::ID,
        route: route_key,
        fill_id: nonce,
        mint: expected_mint,
        amount: delivered,
        expected_amount: committed_minimum,
        source_chain_id,
        created_at: Clock::get()?.unix_timestamp,
        bump: ctx.bumps.verified_fill,
    });

    emit!(CctpMintVerified {
        verified_fill: ctx.accounts.verified_fill.key(),
        route: route_key,
        mint: expected_mint,
        fill_id: nonce,
        amount: delivered,
        expected_amount: committed_minimum,
        source_chain_id,
    });

    Ok(())
}
