use anchor_lang::prelude::*;
use sra_common::{
    constants::MAX_SLIPPAGE_BPS,
    errors::SraError,
    types::{AddressBookEntry, SraConfig, SraMintRoute},
};

/// Partial update to a mint route.
///
/// Every field is `Option<_>` so callers can flip `paused` without touching
/// `target_token` or `allowed_slippage_bps`. `target_token` updates require a
/// fresh enabled `AddressBookEntry` check — mirroring the EVM pattern of
/// re-validating `TokenInfo` whenever the mapping changes.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default)]
pub struct UpdateMintRouteParams {
    pub target_token: Option<[u8; 32]>,
    pub allowed_slippage_bps: Option<u16>,
    pub paused: Option<bool>,
}

#[derive(Accounts)]
pub struct UpdateMintRoute<'info> {
    pub owner: Signer<'info>,
    #[account(has_one = owner)]
    pub sra_config: Account<'info, SraConfig>,
    #[account(mut, has_one = sra_config)]
    pub mint_route: Account<'info, SraMintRoute>,
    /// Required only when `params.target_token` is `Some` (i.e. the token is
    /// being re-pointed). The handler verifies it corresponds to the new
    /// token and is not tombstoned.
    pub address_book_entry: Option<Account<'info, AddressBookEntry>>,
}

pub(crate) fn handler(
    ctx: Context<UpdateMintRoute>,
    params: UpdateMintRouteParams,
) -> Result<()> {
    let mint_route = &mut ctx.accounts.mint_route;
    let sra_config = &ctx.accounts.sra_config;

    if let Some(new_token) = params.target_token {
        // AddressBookEntry is handler-owned, so Anchor's deserializer already
        // verifies the account type/ownership. The stored fields are written
        // in `set_address_book_entry` from the same seeds that derive the PDA,
        // so re-checking them is equivalent to PDA-seed validation.
        let ab = ctx
            .accounts
            .address_book_entry
            .as_ref()
            .ok_or_else(|| error!(SraError::InvalidAddressBook))?;
        require_keys_eq!(ab.sra_config, sra_config.key(), SraError::InvalidAddressBook);
        require!(
            ab.target_chain_id == sra_config.target_chain_id,
            SraError::InvalidAddressBook
        );
        require!(ab.target_token == new_token, SraError::InvalidAddressBook);
        require!(ab.is_enabled(), SraError::InvalidAddressBook);
        mint_route.target_token = new_token;
    }

    if let Some(slippage) = params.allowed_slippage_bps {
        require!(slippage <= MAX_SLIPPAGE_BPS, SraError::InvalidSlippage);
        mint_route.allowed_slippage_bps = slippage;
    }

    if let Some(paused) = params.paused {
        mint_route.paused = paused;
    }

    Ok(())
}
