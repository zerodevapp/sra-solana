use anchor_lang::prelude::*;

#[event]
pub struct CctpDepositExecuted {
    pub sra_config: Pubkey,
    pub input_mint: Pubkey,
    pub nonce: u64,
    /// Ephemeral account holding the CCTP `MessageSent` payload; the off-chain
    /// key for this deposit (Iris assigns the nonce after the fact).
    pub message_sent_event_data: Pubkey,
    pub destination_domain: u32,
    pub input_amount: u64,
    pub output_amount: [u8; 32],
    pub target_token: [u8; 32],
    pub max_fee: u64,
    pub min_finality_threshold: u32,
}

#[event]
pub struct CustodyRecovered {
    pub sra_config: Pubkey,
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}
