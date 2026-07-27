use anchor_lang::prelude::*;

pub mod errors;
pub mod events;

pub use errors::*;

declare_id!("2bXAvxF3U4FtWAnJtcL9LDnwM7qnErf3y3m6USLRKpRw");

#[program]
pub mod sra_cctp_adapter {}
