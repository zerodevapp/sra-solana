# Restricted Token-2022 Output Extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Require explicit route opt-in for TransferHook and NonTransferable Token-2022 outputs without regressing deliberately supported extensions.

**Architecture:** Keep the existing `mint_data_is_permissioned` boundary and replace its wildcard with an exhaustive policy match. Focused packed-mint tests prove the two missing restricted cases; existing integration tests continue to prove the classifier feeds the execution gate.

**Tech Stack:** Rust 1.89.0, Anchor 0.32.1, `spl-token-2022` 8.0.1, Cargo tests, Anchor IDL generation

**Spec:** `docs/superpowers/specs/2026-08-19-permissioned-token-extensions-design.md`

## Global Constraints

- Add no new code comments unless an invisible precondition or non-obvious ordering is required for correctness.
- Preserve instruction arguments, account layouts, route hashes, dependencies, and existing error codes.
- Classify `TransferHook` and `NonTransferable` as restricted output extensions.
- Preserve `TransferFeeConfig` and the other currently accepted mint extensions.
- Match every production `ExtensionType` variant explicitly; do not use a wildcard arm.
- Treat account-only and uninitialized extension variants encountered during mint scanning as `ActionAccountMismatch`.
- Use `NO_DNA=1` for Anchor and Solana CLI commands.

---

### Task 1: Make the Token-2022 output policy exhaustive

**Files:**
- Modify: `programs/sra-handler/src/instructions/destination/verified_engine.rs:348-384`
- Test: `programs/sra-handler/src/instructions/destination/verified_engine.rs:569-646`

**Interfaces:**
- Consumes: `mint_data_is_permissioned(data: &[u8]) -> Result<bool>` and the existing `packed_mint` test helper.
- Produces: `mint_data_is_permissioned` returns `true` for `TransferHook` and `NonTransferable`, preserves accepted mint extensions, and errors on impossible account-only variants.

- [ ] **Step 1: Add the two failing packed-mint tests**

Add these imports beside the existing extension test imports:

```rust
use anchor_spl::token_2022::spl_token_2022::extension::non_transferable::NonTransferable;
use anchor_spl::token_2022::spl_token_2022::extension::transfer_hook::TransferHook;
```

Add these tests beside the other permissioned-mint tests:

```rust
#[test]
fn transfer_hook_mint_is_permissioned() {
    let data = packed_mint(&[ExtensionType::TransferHook], |m| {
        m.init_extension::<TransferHook>(true).unwrap();
    });
    assert!(mint_data_is_permissioned(&data).unwrap());
}

#[test]
fn non_transferable_mint_is_permissioned() {
    let data = packed_mint(&[ExtensionType::NonTransferable], |m| {
        m.init_extension::<NonTransferable>(true).unwrap();
    });
    assert!(mint_data_is_permissioned(&data).unwrap());
}
```

- [ ] **Step 2: Run the TransferHook test and verify RED**

Run:

```bash
cargo test -p sra-handler --lib transfer_hook_mint_is_permissioned
```

Expected: FAIL because `mint_data_is_permissioned` returns `false`.

- [ ] **Step 3: Run the NonTransferable test and verify RED**

Run:

```bash
cargo test -p sra-handler --lib non_transferable_mint_is_permissioned
```

Expected: FAIL because `mint_data_is_permissioned` returns `false`.

- [ ] **Step 4: Replace the wildcard classifier with the exhaustive policy**

Replace the match inside `mint_data_is_permissioned` with:

```rust
match ext {
    ExtensionType::PermanentDelegate
    | ExtensionType::Pausable
    | ExtensionType::TransferHook
    | ExtensionType::NonTransferable => return Ok(true),
    ExtensionType::DefaultAccountState => {
        let das = mint
            .get_extension::<DefaultAccountState>()
            .map_err(|_| error!(SraError::ActionAccountMismatch))?;
        if das.state == AccountState::Frozen as u8 {
            return Ok(true);
        }
    }
    ExtensionType::TransferFeeConfig
    | ExtensionType::MintCloseAuthority
    | ExtensionType::ConfidentialTransferMint
    | ExtensionType::InterestBearingConfig
    | ExtensionType::ConfidentialTransferFeeConfig
    | ExtensionType::MetadataPointer
    | ExtensionType::TokenMetadata
    | ExtensionType::GroupPointer
    | ExtensionType::TokenGroup
    | ExtensionType::GroupMemberPointer
    | ExtensionType::TokenGroupMember
    | ExtensionType::ConfidentialMintBurn
    | ExtensionType::ScaledUiAmount => {}
    ExtensionType::Uninitialized
    | ExtensionType::TransferFeeAmount
    | ExtensionType::ConfidentialTransferAccount
    | ExtensionType::ImmutableOwner
    | ExtensionType::MemoTransfer
    | ExtensionType::CpiGuard
    | ExtensionType::NonTransferableAccount
    | ExtensionType::TransferHookAccount
    | ExtensionType::ConfidentialTransferFeeAmount
    | ExtensionType::PausableAccount => {
        return Err(error!(SraError::ActionAccountMismatch));
    }
}
```

- [ ] **Step 5: Run the focused classifier tests and verify GREEN**

Run:

```bash
cargo test -p sra-handler --lib mint_is_permissioned
```

Expected: all permissioned-mint classifier tests pass, including the two new cases.

- [ ] **Step 6: Run all handler library tests**

Run:

```bash
cargo test -p sra-handler --lib
```

Expected: 41 tests pass with no failures.

- [ ] **Step 7: Check Rust formatting**

Run:

```bash
cargo fmt --all -- --check
```

Expected: exit 0.

- [ ] **Step 8: Commit the classifier and regression tests**

```bash
git add programs/sra-handler/src/instructions/destination/verified_engine.rs
git commit -m "fix: reject restricted Token-2022 outputs"
```

---

### Task 2: Align public policy text and generated IDLs

**Files:**
- Modify: `programs/sra-handler/src/instructions/destination/verified_engine.rs:348-351`
- Modify: `programs/sra-handler/src/instructions/destination/execute_actions.rs:291-293`
- Modify: `programs/sra-handler/tests/verified_cpi_actions.rs:7-9`
- Modify: `crates/sra-common/src/errors.rs:153`
- Modify: `crates/sra-common/src/types/destination.rs:32`
- Modify: `before-audit.md:285`
- Modify: `.github/copilot-instructions.md:64-67`
- Regenerate: `idl/sra_handler.json`
- Regenerate: `idl/sra_across_adapter.json`

**Interfaces:**
- Consumes: the exhaustive restricted-output policy produced by Task 1.
- Produces: consistent source documentation, error text, audit guidance, reviewer guidance, and checked-in Anchor IDLs using the phrase “issuer-controlled or transfer-restricted”.

- [ ] **Step 1: Update policy wording without adding explanatory comments**

Make these exact source changes:

- Change the `mint_is_permissioned` documentation to:

```rust
/// Whether `mint_info` carries an extension that lets an issuer control or block
/// movement of held tokens. Holding such an output is a custody hazard the route
/// owner must opt into (`allow_permissioned_output`). Legacy SPL mints carry no
/// extensions.
```

- Delete the three-line `Safety gate` comment immediately above the existing
  `require!` in `execute_actions.rs`.
- Change the `verified_cpi_actions.rs` module documentation to say that an
  “issuer-controlled or transfer-restricted Token-2022 output requires an
  explicit opt-in.”
- Change the `PermissionedOutputNotAllowed` message to:

```rust
#[msg("Output mint is issuer-controlled or transfer-restricted and the action did not opt in")]
```

- Change the action field documentation to:

```rust
/// Opt-in for issuer-controlled or transfer-restricted Token-2022 output mints.
```

- Update `before-audit.md` to state that restricted outputs may be frozen,
  clawed back, vetoed by a transfer hook, or non-transferable and require opt-in.
- Update `.github/copilot-instructions.md` to require opt-in for
  issuer-controlled or transfer-restricted mints, including permanent delegate,
  pausable, default-frozen, transfer-hook, and non-transferable mints.

- [ ] **Step 2: Generate the Anchor IDLs**

Run:

```bash
NO_DNA=1 anchor build
```

Expected: both programs build and fresh IDLs appear under `target/idl/`.

- [ ] **Step 3: Copy generated IDLs into the checked-in artifact directory**

Run:

```bash
cp target/idl/sra_handler.json idl/sra_handler.json
cp target/idl/sra_across_adapter.json idl/sra_across_adapter.json
```

- [ ] **Step 4: Verify the IDL changes are documentation-only and current**

Run:

```bash
git diff -- idl/sra_handler.json idl/sra_across_adapter.json
diff -u <(jq --sort-keys . idl/sra_handler.json) <(jq --sort-keys . target/idl/sra_handler.json)
diff -u <(jq --sort-keys . idl/sra_across_adapter.json) <(jq --sort-keys . target/idl/sra_across_adapter.json)
```

Expected: the git diff changes only the `allow_permissioned_output` documentation
string, and both comparisons exit 0.

- [ ] **Step 5: Run focused verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p sra-handler --lib
git diff --check
```

Expected: formatting passes, 41 library tests pass, and the diff has no whitespace errors.

- [ ] **Step 6: Commit documentation and generated artifacts**

```bash
git add .github/copilot-instructions.md before-audit.md crates/sra-common/src/errors.rs crates/sra-common/src/types/destination.rs programs/sra-handler/src/instructions/destination/execute_actions.rs programs/sra-handler/src/instructions/destination/verified_engine.rs programs/sra-handler/tests/verified_cpi_actions.rs idl/sra_handler.json idl/sra_across_adapter.json
git commit -m "docs: define restricted Token-2022 outputs"
```

---

## Final Verification

After both tasks and their reviews are complete, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
NO_DNA=1 anchor build
cargo test --workspace
git diff --check origin/main...HEAD
```

Expected: every command exits 0, generated IDLs remain identical to their
checked-in counterparts, and the branch contains only the approved restricted
output fix, tests, and policy documentation.
