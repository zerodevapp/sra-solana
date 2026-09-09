use anchor_lang::prelude::*;

pub const SEED_SOURCE_POLICY: &[u8] = b"source_policy";
pub const SEED_DESTINATION_POLICY: &[u8] = b"destination_policy";
pub const SEED_CCTP_RECEIPT: &[u8] = b"cctp_receipt";
pub const SEED_EXECUTION_LANE: &[u8] = b"execution_lane";
pub const SEED_CCTP_CALLER: &[u8] = b"cctp_caller";
pub const MANAGED_CHAIN_ID: u64 = 42_161;
pub const MANAGED_CCTP_DOMAIN: u32 = 3;
pub const MANAGED_USDC: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xaf, 0x88, 0xd0, 0x65, 0xe7, 0x7c, 0x8c, 0xc2, 0x23, 0x93,
    0x27, 0xc5, 0xed, 0xb3, 0xa4, 0x32, 0x26, 0x8e, 0x58, 0x31,
];

#[account]
#[derive(InitSpace)]
pub struct SourcePolicy {
    pub sra_config: Pubkey,
    pub mint: Pubkey,
    pub salt: [u8; 32],
    pub executor: Pubkey,
    pub paused: bool,
    pub nonce: u64,
    pub finality: u32,
    pub min_gross: u64,
    pub max_gross: u64,
    pub max_fee: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct DestinationPolicy {
    pub handler_route: Pubkey,
    pub mint: Pubkey,
    pub salt: [u8; 32],
    pub sender: [u8; 32],
    pub source_token: [u8; 32],
    pub source_domain: u32,
    pub finality: u32,
    pub max_fee: u64,
    pub min_burn: u64,
    pub max_burn: u64,
    pub queued_liability: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct CctpReceipt {
    pub destination_policy: Pubkey,
    pub nonce: [u8; 32],
    pub amount: u64,
    pub fee_executed: u64,
    pub finality_executed: u32,
    pub message_hash: [u8; 32],
    pub status: ReceiptStatus,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq, Debug, InitSpace)]
pub enum ReceiptStatus {
    Queued,
    Active,
    Released,
    Recovered,
}

#[account]
#[derive(InitSpace)]
pub struct ExecutionLane {
    pub handler_program: Pubkey,
    pub handler_route: Pubkey,
    pub active: Option<ActiveFill>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq, Debug, InitSpace)]
pub struct ActiveFill {
    pub destination_policy: Pubkey,
    pub cctp_receipt: Pubkey,
    pub handler_receipt: Pubkey,
}
