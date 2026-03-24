use anchor_lang::prelude::*;

use crate::constants::{MAX_ACTION_IDXS, MAX_ADAPTERS};

/// One SRA per user per app. Per-mint routing via SraMintRoute child PDAs.
/// PDA seeds: ["sra", owner, config_hash, salt]
/// config_hash = keccak256(borsh(owner, target_chain_id, target_handler, target_adapter, recipient, app_id))
#[account]
#[derive(InitSpace)]
pub struct SraConfig {
    // Immutable (in config_hash -> in PDA)
    pub owner: Pubkey,
    pub target_chain_id: u64,
    pub target_handler: [u8; 32],
    pub target_adapter: [u8; 32],
    pub recipient: [u8; 32],
    pub app_id: [u8; 32],
    // Immutable (NOT in config_hash, in PDA seeds directly)
    pub salt: [u8; 32],
    // Mutable
    #[max_len(MAX_ADAPTERS)]
    pub allowed_adapters: Vec<Pubkey>,
    pub bump: u8,
}

/// Per-mint routing config. One route per (SRA, mint) pair.
/// PDA seeds: ["mint_route", sra_config, source_mint]
#[account]
#[derive(InitSpace)]
pub struct SraMintRoute {
    // Immutable
    pub sra_config: Pubkey,
    pub source_mint: Pubkey,
    #[max_len(MAX_ACTION_IDXS)]
    pub action_idxs: Vec<u64>,
    // Mutable (owner-gated)
    pub target_token: [u8; 32],
    pub allowed_slippage_bps: u16,
    pub paused: bool,
    pub bump: u8,
}

/// Persistent nonce. Never closed — survives route replacement.
/// PDA seeds: ["mint_nonce", sra_config, source_mint]
#[account]
#[derive(InitSpace)]
pub struct MintNonce {
    pub nonce: u64,
    pub bump: u8,
}

/// Admin-managed (adapter, mint, chain, token) registry.
/// PDA seeds: ["address_book", adapter, mint, chain_id.to_le_bytes(), target_token]
#[account]
#[derive(InitSpace)]
pub struct AddressBookEntry {
    pub adapter: Pubkey,
    pub mint: Pubkey,
    pub target_chain_id: u64,
    pub target_token: [u8; 32],
    pub active: bool,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sra_config_init_space() {
        // 32 owner + 8 target_chain_id + 32 target_handler + 32 target_adapter
        // + 32 recipient + 32 app_id + 32 salt + 4 + (32 * 8) allowed_adapters + 1 bump
        // = 32 + 8 + 32 + 32 + 32 + 32 + 32 + 260 + 1 = 461
        assert_eq!(SraConfig::INIT_SPACE, 461);
    }

    #[test]
    fn sra_mint_route_init_space() {
        // 32 sra_config + 32 source_mint + 4 + (8 * 4) action_idxs
        // + 32 target_token + 2 allowed_slippage_bps + 1 paused + 1 bump
        // = 32 + 32 + 36 + 32 + 2 + 1 + 1 = 136
        assert_eq!(SraMintRoute::INIT_SPACE, 136);
    }

    #[test]
    fn mint_nonce_init_space() {
        // 8 nonce + 1 bump = 9
        assert_eq!(MintNonce::INIT_SPACE, 9);
    }

    #[test]
    fn address_book_entry_init_space() {
        // 32 adapter + 32 mint + 8 target_chain_id + 32 target_token + 1 active + 1 bump = 106
        assert_eq!(AddressBookEntry::INIT_SPACE, 106);
    }
}
