//! Across SVM Spoke CPI scaffolding.
//!
//! Exposes the IDL-generated `svm_spoke` module so `execute_deposit_across`
//! can build the `CpiContext` and call `svm_spoke::cpi::deposit_now`
//! directly. Intentionally bare — no wrapper structs or helper functions;
//! the call shape lives in `instructions/execute_deposit.rs`.

// `declare_program!` expands to a `cpi::deposit_now(...)` wrapper with 12
// arguments — past clippy's `too_many_arguments` threshold (7). The generated
// signature is upstream-shaped and out of our control, so suppress at module
// scope (outer `#[allow]` on the macro invocation is ignored by rustc).
#![allow(clippy::too_many_arguments)]

use anchor_lang::declare_program;

declare_program!(svm_spoke);
