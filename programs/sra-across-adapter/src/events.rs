use anchor_lang::prelude::*;

#[event]
pub struct AcrossDepositExecuted {
    pub sra_config: Pubkey,
    pub input_mint: Pubkey,
    pub nonce: u64,
    pub delegate: Pubkey,
    pub across_state: Pubkey,
    pub across_vault: Pubkey,
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_token: [u8; 32],
    pub exclusive_relayer: Pubkey,
    pub fill_deadline_offset: u32,
    pub exclusivity_period: u32,
}

#[event]
pub struct CustodyRecovered {
    pub sra_config: Pubkey,
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

#[event]
pub struct AcrossFillVerifiedAfterClose {
    pub verified_fill: Pubkey,
    pub route: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub fill_id: [u8; 32],
    pub amount: u64,
    pub expected_amount: u64,
    pub source_chain_id: u64,
}

#[event]
pub struct AcrossFillVerified {
    pub verified_fill: Pubkey,
    pub route: Pubkey,
    pub mint: Pubkey,
    pub fill_id: [u8; 32],
    pub amount: u64,
    pub expected_amount: u64,
    pub source_chain_id: u64,
}
