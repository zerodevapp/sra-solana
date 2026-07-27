use anchor_lang::prelude::*;

// MUST match sra-handler's program ID. All #[account] types in this crate
// generate Owner::owner() returning this ID — covers both source and destination
// handler-owned accounts.
declare_id!("7CChUDvMWCtqWPbtCs74CCAgvuJnyiqpgNYEocNJrno5");

pub mod abi;
pub mod constants;
pub mod custody;
pub mod errors;
pub mod fees;
pub mod instruction_data;
pub mod ix_attestation;
pub mod quote;
pub mod seeds;
pub mod source_deposit;
#[cfg(not(target_os = "solana"))]
pub mod test_utils;
pub mod types;
