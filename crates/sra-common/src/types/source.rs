use anchor_lang::prelude::*;

use crate::{
    constants::{MAX_ADAPTERS, MAX_SLIPPAGE_BPS, SOLANA_CHAIN_ID},
    errors::SraError,
};

/// Owner-installed source policy. Committed at SRA install via
/// `handler_data_hash` (folded into the SRA's PDA identity) and re-supplied,
/// then re-authenticated, on every deposit. The routing tuple
/// (`target_chain_id` / `target_handler` / `target_adapter` / `recipient` /
/// `target_token`) is part of the immutable identity, mirroring the
/// EVM `RouteConfig` that `trigger()` reads from storage and a caller cannot
/// redirect (see `sra-stx/src/handler/SRAHandler.sol`). Every deposit must echo
/// this exact routing in `PrepareParams`; `validate_and_prepare` rejects any
/// divergence. Changing the route means a new SRA at a new address — the Solana
/// analogue of the EVM clone-proxy + `_onInstall` model.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq, InitSpace)]
pub struct SourceHandlerData {
    pub address_book_root: Pubkey,
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub target_token: [u8; 32],
    pub allowed_slippage_bps: u16,
    #[max_len(MAX_ADAPTERS)]
    pub allowed_adapters: Vec<Pubkey>,
}

impl SourceHandlerData {
    pub fn validate(&self) -> Result<()> {
        require!(
            self.allowed_adapters.len() <= MAX_ADAPTERS,
            SraError::TooManyAdapters
        );
        require!(
            self.allowed_slippage_bps <= MAX_SLIPPAGE_BPS,
            SraError::InvalidSlippage
        );
        require!(
            self.target_handler != [0u8; 32]
                && self.target_adapter != [0u8; 32]
                && self.recipient != [0u8; 32]
                && self.target_token != [0u8; 32]
                && self.target_chain_id != 0
                && self.target_chain_id != SOLANA_CHAIN_ID,
            SraError::InvalidRouteField
        );
        Ok(())
    }

    pub fn hash(&self) -> [u8; 32] {
        crate::seeds::handler_data_hash(self)
    }

    pub fn config_hash(&self, owner: &Pubkey, final_handler: &Pubkey) -> [u8; 32] {
        crate::seeds::config_hash(owner, final_handler, &self.hash())
    }
}

/// One source-side SRA per `(owner, final_handler, handler_data_hash, salt)`.
/// PDA seeds: ["sra", owner, config_hash, salt]
/// config_hash = keccak256(owner ‖ final_handler ‖ handler_data_hash)
#[account]
#[derive(InitSpace)]
pub struct SraConfig {
    pub owner: Pubkey,
    pub final_handler: Pubkey,
    pub handler_data_hash: [u8; 32],
    pub salt: [u8; 32],
    pub bump: u8,
}

/// Owner-managed source address-book namespace. The root account is initialized
/// explicitly and then committed inside immutable `SourceHandlerData`.
#[account]
#[derive(InitSpace)]
pub struct AddressBookRoot {
    pub owner: Pubkey,
}

/// Persistent nonce per `(sra_config, source_mint)`. The owner-signed deposit
/// flow will create it lazily and increment it per source-mint deposit.
/// PDA seeds: ["mint_nonce", sra_config, source_mint]
#[account]
#[derive(InitSpace)]
pub struct MintNonce {
    pub nonce: u64,
    pub bump: u8,
}

/// Per-address-book-root owner-managed (source_mint, chain, target_token) -> decimals registry.
/// `target_decimals = 0` is a tombstone/disabled entry, mirroring zeroed EVM TokenInfo.
/// PDA seeds: ["address_book", address_book_root, source_mint, chain_id.to_le_bytes(), target_token]
#[account]
#[derive(InitSpace)]
pub struct AddressBookEntry {
    pub address_book_root: Pubkey,
    pub source_mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    /// Decimals of `target_token` on the target chain. Serves three roles:
    /// (a) canonical record for downstream amount math, (b) EVM-parity
    /// existence sentinel, (c) tombstone marker when `0`. Tokens with 0
    /// real decimals are not representable here — not a concern for USDC
    /// and other bridgeable tokens, but worth knowing.
    pub target_decimals: u8,
    pub bump: u8,
}

impl AddressBookEntry {
    /// `true` iff the entry is not a tombstone. All consumers that need to
    /// gate on "is this (chain, token) currently allowed?" should route
    /// through this helper rather than inlining `target_decimals != 0`.
    pub fn is_enabled(&self) -> bool {
        self.target_decimals != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_handler_data_init_space() {
        // 32 address_book_root + 8 target_chain_id + 32 target_handler
        // + 32 target_adapter + 32 recipient + 32 target_token
        // + 2 allowed_slippage_bps + 4 + (32 * 8) allowed_adapters = 430
        assert_eq!(SourceHandlerData::INIT_SPACE, 430);
    }

    #[test]
    fn sra_config_init_space() {
        // 32 owner + 32 final_handler + 32 handler_data_hash + 32 salt + 1 bump = 129
        assert_eq!(SraConfig::INIT_SPACE, 129);
    }

    #[test]
    fn address_book_root_init_space() {
        // 32 owner
        assert_eq!(AddressBookRoot::INIT_SPACE, 32);
    }

    #[test]
    fn mint_nonce_init_space() {
        // 8 nonce + 1 bump = 9
        assert_eq!(MintNonce::INIT_SPACE, 9);
    }

    #[test]
    fn address_book_entry_init_space() {
        // 32 address_book_root + 32 source_mint + 8 target_chain_id + 32 target_token + 1 target_decimals + 1 bump = 106
        assert_eq!(AddressBookEntry::INIT_SPACE, 106);
    }
}
