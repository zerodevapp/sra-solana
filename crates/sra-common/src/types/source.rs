use anchor_lang::prelude::*;

use crate::{
    constants::{MAX_ADAPTERS, MAX_SLIPPAGE_BPS, SOLANA_CHAIN_ID},
    errors::SraError,
};

/// Owner-installed source policy. Stored in the SRA and committed at install
/// via `handler_data_hash`, which is folded into the SRA's PDA identity. The
/// routing tuple
/// (`target_chain_id` / `recipient` / `target_token`) is part of the immutable
/// identity, mirroring the EVM `RouteConfig` that `trigger()` reads from
/// storage and a caller cannot redirect. Every deposit must echo this exact
/// routing in `PrepareParams`; `trigger` rejects any divergence.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq, InitSpace)]
pub struct SourceHandlerData {
    pub address_book_root: Pubkey,
    pub target_chain_id: u64,
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
            self.recipient != [0u8; 32]
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
    pub handler_data: SourceHandlerData,
    pub salt: [u8; 32],
    pub initiator: Pubkey,
    pub active: bool,
    pub deposit_nonce: u64,
    pub bump: u8,
}

impl SraConfig {
    pub fn last_triggered_nonce(&self) -> Option<u64> {
        self.deposit_nonce.checked_sub(1)
    }
}

/// Owner-managed source address-book namespace. The root account is initialized
/// explicitly and then committed inside immutable `SourceHandlerData`.
#[account]
#[derive(InitSpace)]
pub struct AddressBookRoot {
    pub owner: Pubkey,
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

    fn sample_sra_config(deposit_nonce: u64) -> SraConfig {
        SraConfig {
            owner: Pubkey::new_unique(),
            final_handler: Pubkey::new_unique(),
            handler_data: SourceHandlerData {
                address_book_root: Pubkey::new_unique(),
                target_chain_id: 42_161,
                recipient: [0x33; 32],
                target_token: [0xAB; 32],
                allowed_slippage_bps: 50,
                allowed_adapters: vec![Pubkey::new_unique()],
            },
            salt: [0x55; 32],
            initiator: Pubkey::new_unique(),
            active: true,
            deposit_nonce,
            bump: 254,
        }
    }

    #[test]
    fn last_triggered_nonce_handles_zero_and_positive_boundaries() {
        assert_eq!(sample_sra_config(0).last_triggered_nonce(), None);
        assert_eq!(sample_sra_config(1).last_triggered_nonce(), Some(0));
        assert_eq!(sample_sra_config(7).last_triggered_nonce(), Some(6));
    }

    #[test]
    fn source_handler_data_init_space() {
        // 32 address_book_root + 8 target_chain_id + 32 recipient + 32 target_token
        // + 2 allowed_slippage_bps + 4 + (32 * 8) allowed_adapters = 366
        assert_eq!(SourceHandlerData::INIT_SPACE, 366);
    }

    #[test]
    fn sra_config_init_space() {
        assert_eq!(
            SraConfig::INIT_SPACE,
            32 + 32 + SourceHandlerData::INIT_SPACE + 32 + 32 + 1 + 8 + 1,
        );
    }

    #[test]
    fn address_book_root_init_space() {
        // 32 owner
        assert_eq!(AddressBookRoot::INIT_SPACE, 32);
    }

    #[test]
    fn address_book_entry_init_space() {
        // 32 address_book_root + 32 source_mint + 8 target_chain_id + 32 target_token + 1 target_decimals + 1 bump = 106
        assert_eq!(AddressBookEntry::INIT_SPACE, 106);
    }
}
