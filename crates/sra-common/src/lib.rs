use anchor_lang::prelude::*;

// MUST match sra-handler's program ID. All #[account] types in this crate
// generate Owner::owner() returning this ID — covers both source and destination
// handler-owned accounts.
declare_id!("Gte7LViTnE2UixBF59Xfu2sSqcmHhMNANj42yUkYpkdo");

pub mod abi;
pub mod constants;
pub mod errors;
pub mod instruction_data;
pub mod seeds;
pub mod source_deposit;
#[cfg(not(target_os = "solana"))]
pub mod test_utils;
pub mod types;
