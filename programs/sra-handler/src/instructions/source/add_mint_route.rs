use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
use sra_common::{
    constants::{MAX_ACTION_IDXS, MAX_SLIPPAGE_BPS},
    errors::SraError,
    seeds::{SEED_ADAPTER_AUTHORITY, SEED_ADDRESS_BOOK, SEED_MINT_NONCE, SEED_MINT_ROUTE},
    types::{AddressBookEntry, MintNonce, SraConfig, SraMintRoute},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct AddMintRouteParams {
    pub action_idxs: Vec<u64>,
    pub target_token: [u8; 32],
    pub allowed_slippage_bps: u16,
}

/// Creates an `SraMintRoute` for `(sra_config, source_mint)` and its vault ATAs.
///
/// `MintNonce` is `init_if_needed` so the nonce persists across a
/// `close_mint_route` + `add_mint_route` cycle — replay hashes on the EVM side
/// are keyed off the nonce, so resetting it would allow collisions.
///
/// The `AddressBookEntry` PDA is derived from `params.target_token` and
/// `sra_config.target_chain_id`; a disabled/tombstoned entry
/// (`target_decimals == 0`) is rejected, matching the EVM check in
/// `SRAHandler._deposit` that guards against zeroed `TokenInfo`.
///
/// `adapter_authority` is validated as
/// `PDA(["adapter_authority", sra_config], adapter_program)` so the freshly
/// created `adapter_custody` ATA is guaranteed to be spendable by the intended
/// adapter — not a stranded account.
#[derive(Accounts)]
#[instruction(params: AddMintRouteParams)]
pub struct AddMintRoute<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
    #[account(
        init,
        payer = payer,
        space = 8 + SraMintRoute::INIT_SPACE,
        seeds = [
            SEED_MINT_ROUTE,
            sra_config.key().as_ref(),
            source_mint.key().as_ref(),
        ],
        bump,
    )]
    pub mint_route: Account<'info, SraMintRoute>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + MintNonce::INIT_SPACE,
        seeds = [
            SEED_MINT_NONCE,
            sra_config.key().as_ref(),
            source_mint.key().as_ref(),
        ],
        bump,
    )]
    pub mint_nonce: Account<'info, MintNonce>,
    pub source_mint: Account<'info, Mint>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = source_mint,
        associated_token::authority = sra_config,
    )]
    pub sra_vault: Account<'info, TokenAccount>,
    #[account(
        has_one = sra_config,
        seeds = [
            SEED_ADDRESS_BOOK,
            sra_config.key().as_ref(),
            &sra_config.target_chain_id.to_le_bytes(),
            &params.target_token,
        ],
        bump = address_book_entry.bump,
    )]
    pub address_book_entry: Account<'info, AddressBookEntry>,
    /// CHECK: validated below as the adapter_authority PDA under `adapter_program`.
    #[account(
        seeds = [SEED_ADAPTER_AUTHORITY, sra_config.key().as_ref()],
        bump,
        seeds::program = adapter_program.key(),
    )]
    pub adapter_authority: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = source_mint,
        associated_token::authority = adapter_authority,
    )]
    pub adapter_custody: Account<'info, TokenAccount>,
    /// CHECK: program id used only for deriving `adapter_authority`.
    pub adapter_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<AddMintRoute>, params: AddMintRouteParams) -> Result<()> {
    require!(
        params.action_idxs.len() <= MAX_ACTION_IDXS,
        SraError::TooManyActionIdxs
    );
    require!(
        params.allowed_slippage_bps <= MAX_SLIPPAGE_BPS,
        SraError::InvalidSlippage
    );
    require!(
        ctx.accounts.address_book_entry.is_enabled(),
        SraError::InvalidAddressBook
    );

    let mint_route = &mut ctx.accounts.mint_route;
    mint_route.sra_config = ctx.accounts.sra_config.key();
    mint_route.source_mint = ctx.accounts.source_mint.key();
    mint_route.action_idxs = params.action_idxs;
    mint_route.target_token = params.target_token;
    mint_route.allowed_slippage_bps = params.allowed_slippage_bps;
    mint_route.paused = false;
    mint_route.bump = ctx.bumps.mint_route;

    // `init_if_needed`: freshly-initialized state is zero-filled, so a new
    // MintNonce has nonce=0 and bump=0. An existing MintNonce (route was
    // closed and is being recreated) keeps its nonce as-is. Setting `bump`
    // unconditionally is idempotent — same seeds yield the same bump.
    let mint_nonce = &mut ctx.accounts.mint_nonce;
    mint_nonce.bump = ctx.bumps.mint_nonce;

    Ok(())
}
