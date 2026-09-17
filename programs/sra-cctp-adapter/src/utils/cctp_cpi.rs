//! Circle CCTP v2 TokenMessengerMinter CPI scaffolding.
//!
//! Exposes the IDL-generated `token_messenger_minter_v2` module so
//! `execute_deposit_cctp` can build the `CpiContext` and call
//! `token_messenger_minter_v2::cpi::deposit_for_burn` directly. The IDL is
//! vendored from `circlefin/solana-cctp-contracts@ec16e95`
//! (`idls/token_messenger_minter_v2.json`); re-fetch and drift-check it
//! against the deployed program whenever Circle ships a new release.

// `declare_program!` expands to a `cpi::deposit_for_burn(ctx, params)` wrapper
// plus the full generated surface; the upstream shapes are out of our control,
// so suppress the lint at module scope.
#![allow(clippy::too_many_arguments)]

use anchor_lang::{declare_program, prelude::anchor_lang};

declare_program!(token_messenger_minter_v2);
