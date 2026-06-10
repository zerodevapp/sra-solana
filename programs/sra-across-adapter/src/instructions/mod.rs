pub mod execute_deposit;
pub mod recover_custody;

// Both modules export a `handler`; lib.rs calls them by fully-qualified path,
// so the ambiguous glob symbol is never used.
#[allow(ambiguous_glob_reexports)]
pub use execute_deposit::*;
#[allow(ambiguous_glob_reexports)]
pub use recover_custody::*;
