//! Across SVM Spoke CPI scaffolding.
//!
//! Exposes the IDL-generated `svm_spoke` module so Step 15's
//! `execute_deposit_across` can build the `CpiContext` and call
//! `svm_spoke::cpi::deposit_now` directly. Intentionally bare — no wrapper
//! structs or helper functions until Step 15 picks the call shape.

use anchor_lang::declare_program;

declare_program!(svm_spoke);
