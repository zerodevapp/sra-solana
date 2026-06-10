pub mod execute_deposit;
pub mod recover_custody;

// Both instruction modules expose a `handler` fn; the program entrypoints in
// `lib.rs` reference them via fully-qualified paths, so the glob `handler`
// symbol is never used. Silence the (harmless) ambiguity rather than rename.
#[allow(ambiguous_glob_reexports)]
pub use execute_deposit::*;
#[allow(ambiguous_glob_reexports)]
pub use recover_custody::*;
