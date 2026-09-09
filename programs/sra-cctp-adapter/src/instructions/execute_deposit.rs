use anchor_lang::{prelude::*, solana_program::sysvar::instructions};
use anchor_spl::token::{Mint, Token, TokenAccount};
use sra_common::{
    constants::{
        cctp_domain_for_chain, CCTP_MESSAGE_TRANSMITTER_V2_ID, CCTP_TOKEN_MESSENGER_MINTER_V2_ID,
        FINALITY_THRESHOLD_FAST, FINALITY_THRESHOLD_FINALIZED,
    },
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_MINT_NONCE},
    source_deposit::{decode_output_amount, CctpDepositParams, PrepareParams},
    types::{MintNonce, SraConfig},
};

use crate::events::CctpDepositExecuted;
use crate::utils::token_messenger_minter_v2::{cpi as cctp_cpi, types::DepositForBurnParams};
use crate::CctpAdapterError;

// Recover the nonce captured before `validate_and_prepare` increments it.
fn prepared_nonce(mint_nonce: &MintNonce) -> Result<u64> {
    mint_nonce
        .nonce
        .checked_sub(1)
        .ok_or(error!(CctpAdapterError::PreparedNonceUnavailable))
}

pub(crate) fn validate_finality_threshold(min_finality_threshold: u32) -> Result<()> {
    require!(
        matches!(
            min_finality_threshold,
            FINALITY_THRESHOLD_FAST | FINALITY_THRESHOLD_FINALIZED
        ),
        CctpAdapterError::InvalidFinalityThreshold
    );
    Ok(())
}

/// The committed quote must survive the worst-case CCTP fee: the destination
/// delivers `net − fee_executed` with `fee_executed ≤ max_fee`, so require
/// `net − max_fee ≥ output` (fails closed when `max_fee` exceeds the net).
///
/// The comparison is in raw token units, which is exact only because CCTP is
/// 1:1 and USDC is 6-decimals on every supported domain; revisit if a non-6dp
/// corridor is ever added.
fn validate_quote_after_max_fee(prepare: &PrepareParams, max_fee: u64) -> Result<()> {
    let worst_case_delivery = prepare
        .net_input_amount()?
        .checked_sub(max_fee)
        .ok_or(error!(CctpAdapterError::QuoteExceedsNetAfterFee))?;
    require!(
        u128::from(worst_case_delivery) >= decode_output_amount(prepare.output_amount)?,
        CctpAdapterError::QuoteExceedsNetAfterFee
    );
    Ok(())
}

#[derive(Accounts)]
pub struct ExecuteDepositCctp<'info> {
    /// Pays the tx fee and the CCTP `MessageSent` event-account rent.
    #[account(mut)]
    pub fee_payer: Signer<'info>,
    /// CHECK: Instructions sysvar used for paired-instruction attestation.
    #[account(address = instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub sra_config: Box<Account<'info, SraConfig>>,
    #[account(
        seeds = [SEED_MINT_NONCE, sra_config.key().as_ref(), input_mint.key().as_ref()],
        bump = mint_nonce.bump,
        seeds::program = sra_common::ID,
    )]
    pub mint_nonce: Box<Account<'info, MintNonce>>,
    /// CHECK: adapter authority PDA under this program; the handler derives
    /// the same address via `seeds::program = adapter_program.key()`. Owns
    /// `adapter_custody` and signs the burn directly (no delegate approval).
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = input_mint,
        associated_token::authority = adapter_authority,
    )]
    pub adapter_custody: Box<Account<'info, TokenAccount>>,
    /// The burn decreases the supply, so the mint is writable.
    #[account(mut)]
    pub input_mint: Box<Account<'info, Mint>>,
    /// CHECK: CCTP burn-signing authority PDA, cheap to pin by seeds.
    #[account(seeds = [b"sender_authority"], bump, seeds::program = cctp_program.key())]
    pub sender_authority: UncheckedAccount<'info>,
    /// CHECK: validated by the CCTP program during the CPI.
    #[account(mut)]
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: validated by the CCTP program during the CPI.
    pub token_messenger: UncheckedAccount<'info>,
    /// CHECK: validated by the CCTP program during the CPI (its stored domain
    /// must equal `destination_domain`).
    pub remote_token_messenger: UncheckedAccount<'info>,
    /// CHECK: validated by the CCTP program during the CPI.
    pub token_minter: UncheckedAccount<'info>,
    /// CHECK: validated by the CCTP program during the CPI.
    #[account(mut)]
    pub local_token: UncheckedAccount<'info>,
    /// CHECK: per-owner denylist PDA (must hold no data), cheap to pin by seeds.
    #[account(
        seeds = [b"denylist_account", adapter_authority.key().as_ref()],
        bump,
        seeds::program = cctp_program.key(),
    )]
    pub denylist_account: UncheckedAccount<'info>,
    /// Ephemeral keypair account CCTP initializes with the `MessageSent`
    /// payload; co-signs the transaction, rent paid by `fee_payer`.
    #[account(mut)]
    pub message_sent_event_data: Signer<'info>,
    /// CHECK: canonical CCTP MessageTransmitter v2 program.
    #[account(address = CCTP_MESSAGE_TRANSMITTER_V2_ID)]
    pub message_transmitter_program: UncheckedAccount<'info>,
    /// CHECK: canonical CCTP TokenMessengerMinter v2 program.
    #[account(address = CCTP_TOKEN_MESSENGER_MINTER_V2_ID)]
    pub cctp_program: UncheckedAccount<'info>,
    /// CHECK: CCTP Anchor event-authority PDA, cheap to pin by seeds.
    #[account(seeds = [b"__event_authority"], bump, seeds::program = cctp_program.key())]
    pub cctp_event_authority: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// Executes the adapter leg of a paired source deposit: burns the net input
/// via CCTP v2 `deposit_for_burn` toward the attested recipient.
///
/// Most routing fields in `params` are only cross-checked here (against the
/// handler's attested prepare data); the CCTP-specific execution fields are
/// `max_fee` and `min_finality_threshold`.
pub fn handler(ctx: Context<ExecuteDepositCctp>, params: CctpDepositParams) -> Result<()> {
    validate_finality_threshold(params.min_finality_threshold)?;
    // Derived, never caller-supplied: one less cross-check to get wrong.
    let destination_domain = cctp_domain_for_chain(params.target_chain_id)
        .ok_or(error!(CctpAdapterError::UnsupportedChain))?;

    let prepare = sra_common::ix_attestation::verify_previous_handler_ix(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &ctx.accounts.sra_config.key(),
        &ctx.accounts.input_mint.key(),
        ctx.program_id,
        &params,
    )?;

    validate_quote_after_max_fee(&prepare, params.max_fee)?;

    let prepared_nonce = prepared_nonce(&ctx.accounts.mint_nonce)?;

    let sra_config_key = ctx.accounts.sra_config.key();
    let adapter_authority_bump = [ctx.bumps.adapter_authority];
    let adapter_authority_seeds = sra_common::seeds::adapter_authority_signer_seeds(
        sra_config_key.as_ref(),
        &adapter_authority_bump,
    );
    // The adapter-authority PDA is the burn `owner` and signs the CPI directly
    // (no delegate approval). Account order/flags come from the vendored IDL.
    let cpi_accounts = cctp_cpi::accounts::DepositForBurn {
        owner: ctx.accounts.adapter_authority.to_account_info(),
        event_rent_payer: ctx.accounts.fee_payer.to_account_info(),
        sender_authority_pda: ctx.accounts.sender_authority.to_account_info(),
        burn_token_account: ctx.accounts.adapter_custody.to_account_info(),
        denylist_account: ctx.accounts.denylist_account.to_account_info(),
        message_transmitter: ctx.accounts.message_transmitter.to_account_info(),
        token_messenger: ctx.accounts.token_messenger.to_account_info(),
        remote_token_messenger: ctx.accounts.remote_token_messenger.to_account_info(),
        token_minter: ctx.accounts.token_minter.to_account_info(),
        local_token: ctx.accounts.local_token.to_account_info(),
        burn_token_mint: ctx.accounts.input_mint.to_account_info(),
        message_sent_event_data: ctx.accounts.message_sent_event_data.to_account_info(),
        message_transmitter_program: ctx.accounts.message_transmitter_program.to_account_info(),
        token_messenger_minter_program: ctx.accounts.cctp_program.to_account_info(),
        token_program: ctx.accounts.token_program.to_account_info(),
        system_program: ctx.accounts.system_program.to_account_info(),
        event_authority: ctx.accounts.cctp_event_authority.to_account_info(),
        program: ctx.accounts.cctp_program.to_account_info(),
    };
    cctp_cpi::deposit_for_burn(
        CpiContext::new_with_signer(
            ctx.accounts.cctp_program.to_account_info(),
            cpi_accounts,
            &[&adapter_authority_seeds],
        ),
        DepositForBurnParams {
            amount: params.input_amount,
            destination_domain,
            mint_recipient: Pubkey::new_from_array(params.recipient),
            // Permissionless receive on the destination, mirroring our own
            // receive policy.
            destination_caller: Pubkey::default(),
            max_fee: params.max_fee,
            min_finality_threshold: params.min_finality_threshold,
        },
    )?;

    emit!(CctpDepositExecuted {
        sra_config: ctx.accounts.sra_config.key(),
        input_mint: ctx.accounts.input_mint.key(),
        nonce: prepared_nonce,
        message_sent_event_data: ctx.accounts.message_sent_event_data.key(),
        destination_domain,
        input_amount: params.input_amount,
        output_amount: params.output_amount,
        target_token: params.target_token,
        max_fee: params.max_fee,
        min_finality_threshold: params.min_finality_threshold,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sra_common::constants::{FINALITY_THRESHOLD_FAST, NO_FEE_BPS_OVERRIDE};

    #[test]
    fn prepared_nonce_rejects_zero() {
        let mint_nonce = MintNonce { nonce: 0, bump: 1 };
        let err = prepared_nonce(&mint_nonce).expect_err("zero nonce must reject");
        assert_eq!(err, error!(CctpAdapterError::PreparedNonceUnavailable));
    }

    #[test]
    fn prepared_nonce_uses_pre_increment_value() {
        let mint_nonce = MintNonce { nonce: 7, bump: 1 };
        assert_eq!(prepared_nonce(&mint_nonce).unwrap(), 6);
    }

    #[test]
    fn finality_threshold_bounds() {
        let err = validate_finality_threshold(0).expect_err("0 must reject");
        assert_eq!(err, error!(CctpAdapterError::InvalidFinalityThreshold));
        for value in [1, 999, 1001, 1999] {
            assert!(validate_finality_threshold(value).is_err());
        }
        validate_finality_threshold(FINALITY_THRESHOLD_FAST).expect("fast is canonical");
        validate_finality_threshold(FINALITY_THRESHOLD_FINALIZED).expect("2000 is the upper bound");
        let err = validate_finality_threshold(2001).expect_err("2001 must reject");
        assert_eq!(err, error!(CctpAdapterError::InvalidFinalityThreshold));
    }

    fn quote_prepare(input: u64, fee: u64, output: u64) -> PrepareParams {
        PrepareParams::new(
            input,
            sra_common::abi::u256_from_u64(output),
            42_161,
            [0x33; 32],
            [0xAB; 32],
            fee,
            false,
            NO_FEE_BPS_OVERRIDE,
        )
    }

    #[test]
    fn quote_invariant_boundaries() {
        // net = 100 − 3 = 97; max_fee 2 → worst-case delivery 95.
        validate_quote_after_max_fee(&quote_prepare(100, 3, 95), 2)
            .expect("net − max_fee == output must pass");
        let err = validate_quote_after_max_fee(&quote_prepare(100, 3, 96), 2)
            .expect_err("output one above worst-case delivery must reject");
        assert_eq!(err, error!(CctpAdapterError::QuoteExceedsNetAfterFee));
    }

    #[test]
    fn quote_invariant_max_fee_above_net_fails_closed() {
        let err = validate_quote_after_max_fee(&quote_prepare(100, 3, 0), 98)
            .expect_err("max_fee above the net must fail closed");
        assert_eq!(err, error!(CctpAdapterError::QuoteExceedsNetAfterFee));
    }

    #[test]
    fn quote_invariant_rejects_output_above_u64_range() {
        // A u256 output beyond u64::MAX can never be covered by a u64 net.
        let mut prepare = quote_prepare(u64::MAX, 0, 0);
        prepare.output_amount[15] = 0; // keep decodable…
        prepare.output_amount[16..].copy_from_slice(&(1u128 << 64).to_be_bytes());
        let err = validate_quote_after_max_fee(&prepare, 0)
            .expect_err("output above u64::MAX must reject");
        assert_eq!(err, error!(CctpAdapterError::QuoteExceedsNetAfterFee));
    }
}
