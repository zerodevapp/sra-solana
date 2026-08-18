# Restricted Token-2022 Output Design

**Status:** Approved for implementation

## Problem

`mint_data_is_permissioned` currently treats every Token-2022 extension outside
`PermanentDelegate`, `Pausable`, and default-frozen accounts as safe. That makes
`TransferHook` bypass `allow_permissioned_output`: a hook can permit the inbound
swap and reject a later outbound transfer after the receipt becomes
`ActionsSucceeded`. `NonTransferable` has the same output-immobility consequence
when a target program credits the destination without transferring from another
token account.

The wildcard match also lets a future `ExtensionType` variant become implicitly
safe when the Token-2022 dependency is upgraded.

## Goals

- Require `allow_permissioned_output` for `TransferHook` and `NonTransferable`
  output mints.
- Preserve the existing behavior for transfer-fee and other accepted mint
  extensions.
- Make every pinned `ExtensionType` classification explicit so dependency
  upgrades require a policy decision at compile time.
- Reject account-only extension variants if malformed mint data exposes one to
  the classifier.
- Keep the existing instruction arguments, account layouts, route hashes, and
  error code stable.

## Non-goals

- Redefining the policy for a base mint `freeze_authority`.
- Reclassifying mutable transfer fees, which the engine deliberately supports
  through post-fee balance accounting.
- Adding a hook allowlist, a hook-specific consent field, or a new error variant.
- Building a mode-switchable transfer-hook program for an end-to-end exploit
  reproduction.

## Design

Keep the policy inside `mint_data_is_permissioned`, where both initial execution
and retry already consume it through `run_committed_actions`.

The exhaustive match has three outcomes:

1. Restricted: `PermanentDelegate`, `Pausable`, `TransferHook`, and
   `NonTransferable` return `true`.
2. State-dependent: `DefaultAccountState` returns `true` only when the default is
   frozen.
3. Accepted mint extensions: transfer fee, mint close, confidential transfer,
   interest, confidential fee, metadata, group, confidential mint/burn, and
   scaled UI extensions continue scanning.

`Uninitialized` and account-only variants return `ActionAccountMismatch`. The
match has no wildcard, so adding an upstream enum variant causes a compile error
until it is classified.

## Error and State Flow

The existing `allow_permissioned_output || !mint_is_permissioned(...)` gate stays
in the first validation pass. A restricted mint with opt-in disabled therefore
returns `PermissionedOutputNotAllowed` before any transfer or arbitrary CPI. The
transaction leaves the receipt `Finalized` and the execution vault unchanged.
Opted-in routes retain their current behavior.

## Tests

Add packed-mint unit tests for `TransferHook` and `NonTransferable`. Each test
exercises the real serialized Token-2022 TLV path and fails against the current
wildcard behavior. Existing integration tests already establish that a `true`
classifier result is rejected without opt-in and accepted with opt-in, so another
mock hook program would duplicate gate coverage without improving the regression
signal.

## Documentation and Compatibility

Use the generic phrase “issuer-controlled or transfer-restricted Token-2022
output” in public policy text instead of maintaining an incomplete extension
list. Regenerate both checked-in Anchor IDLs after changing the shared action
field documentation.

This changes validation behavior only for hook and non-transferable output mints
whose routes did not opt in. There is no ABI, serialization, account-layout,
dependency, or error-code change.

## Code Style

Add no explanatory code comments. Remove or generalize stale existing wording;
let names and the exhaustive match express the policy.
