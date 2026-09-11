# Passive Trigger Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every material finding from the request-changes review on PR #108 without changing the intended passive-deposit behavior or adapter event schemas.

**Architecture:** Make stored `SraConfig.handler_data` the sole trigger routing authority, reduce the trigger account list to the accounts used at runtime, and make `TriggerArgs` the single handler/adapter wire type. Move reusable validation and nonce logic to focused shared units, then decompose passive tests by protocol concern.

**Tech Stack:** Rust, Anchor 0.32.1, legacy SPL Token, LiteSVM, generated Anchor IDLs.

**Spec:** `docs/superpowers/specs/2026-09-11-passive-trigger-review-remediation-design.md`

## Global Constraints

- Work on `codex/passive-deposits-task-2` in `.worktrees/passive-deposits-task-2`; PR #108 targets `cctpV2`.
- Preserve the reviewed passive-source behavior, fee semantics, checked per-SRA nonce progression, adapter events, and destination interfaces.
- The final trigger account order has 17 accounts; `adapter_program` is slot 7.
- `SraConfig.handler_data` supplies the address-book root, target chain, recipient, and target token.
- The public trigger entrypoint accepts one `TriggerArgs` value.
- Canonical CCTP input validation pins `input_mint` to `USDC_MINT`; it does not separately inspect decimals.
- Default to zero new code comments. Preserve only Anchor-required safety statements and the optional-account explanation required by the approved design.
- No Task 3 recovery, destination change, adapter event change, deployment, or live transaction is in scope.
- Follow TDD for behavior changes. For pure moves and test decomposition, start from the green characterization suite and keep it green.
- Rebuild every handler, adapter, and mock SBF artifact used by runtime tests from the same revision.

---

### Task 1: Isolate checked source slippage validation

**Files:**
- Modify: `crates/sra-common/src/source_deposit.rs`
- Modify: `programs/sra-handler/src/instructions/source/trigger.rs`

**Interfaces:**
- Produces: `sra_common::source_deposit::output_satisfies_slippage_floor(u64, u8, u128, u8, u16) -> Result<bool>`.
- Preserves: exact ceiling behavior, fail-closed target-scale overflow, mathematically sufficient source-scale overflow, and a zero minimum at 100% allowed slippage.

- [ ] **Step 1: Record the green characterization baseline**

Run:

```bash
cargo test -p sra-handler --lib slippage_floor -- --nocapture
```

Expected: the six current trigger-local slippage tests pass.

- [ ] **Step 2: Move the validator to `sra-common`**

Move the current implementation unchanged into `crates/sra-common/src/source_deposit.rs` as:

```rust
pub fn output_satisfies_slippage_floor(
    input_amount: u64,
    input_decimals: u8,
    output_amount: u128,
    output_decimals: u8,
    slippage_bps: u16,
) -> Result<bool>
```

Import `BPS_DENOMINATOR` there. In `trigger.rs`, import and call this function; remove the local implementation.

- [ ] **Step 3: Move and restore boundary tests**

Move the six validator tests into `source_deposit.rs`. Add:

```rust
#[test]
fn slippage_floor_exact_division_boundary_unchanged() {
    assert!(output_satisfies_slippage_floor(1_000_000, 6, 995_000, 6, 50).unwrap());
    assert!(!output_satisfies_slippage_floor(1_000_000, 6, 994_999, 6, 50).unwrap());
}

#[test]
fn slippage_floor_zero_slippage_requires_full_amount() {
    assert!(output_satisfies_slippage_floor(1_000_000, 6, 1_000_000, 6, 0).unwrap());
    assert!(!output_satisfies_slippage_floor(1_000_000, 6, 999_999, 6, 0).unwrap());
}
```

Before finalizing, temporarily mutate each boundary comparison and confirm its corresponding test fails; restore the implementation immediately.

- [ ] **Step 4: Verify the isolated behavior**

Run:

```bash
cargo test -p sra-common source_deposit::tests::slippage_floor -- --nocapture
cargo test -p sra-handler --lib
cargo fmt --all -- --check
git diff --check
```

Expected: all commands pass and `trigger.rs` contains no slippage implementation or unit-test module.

- [ ] **Step 5: Commit the isolated semantic fix**

```bash
git add crates/sra-common/src/source_deposit.rs programs/sra-handler/src/instructions/source/trigger.rs
git commit -m "fix(common): isolate checked source slippage"
```

---

### Task 2: Simplify the trigger ABI and account contract

**Files:**
- Create: `programs/sra-handler/src/utils/program_account.rs`
- Modify: `programs/sra-handler/src/utils/mod.rs`
- Modify: `programs/sra-handler/src/instructions/source/trigger.rs`
- Modify: `programs/sra-handler/src/lib.rs`
- Modify: `crates/sra-common/src/ix_attestation.rs`
- Modify: `programs/sra-handler/tests/common/mod.rs`
- Modify: `programs/sra-handler/tests/passive_source.rs`
- Modify: `programs/sra-handler/tests/source_cu_budget.rs`
- Modify: `programs/sra-handler/tests/source_e2e_surfpool.rs`
- Modify: `programs/sra-handler/tests/source_tx_size.rs`
- Modify: `idl/sra_handler.json`
- Verify: `idl/sra_cctp_adapter.json`
- Verify: `idl/sra_across_adapter.json`

**Interfaces:**
- Consumes: `TriggerArgs { params, paired_adapter_ix_data_hash }` from `sra-common`.
- Produces: a 17-account trigger with `adapter_program` at slot 7 and `address_book_entry` at slot 8.

- [ ] **Step 1: Write the failing slot-7 attestation test**

In `crates/sra-common/src/ix_attestation.rs`, change the test instruction fixture to the desired eight-account prefix:

```text
0 fee_payer, 1 initiator, 2 sra_config, 3 source_mint,
4 sra_vault, 5 adapter_authority, 6 adapter_custody, 7 adapter_program
```

Keep a valid `TriggerArgs` payload and run:

```bash
cargo test -p sra-common ix_attestation -- --nocapture
```

Expected RED: valid CCTP and Across cases fail because production still requires slot 8 and at least nine accounts.

- [ ] **Step 2: Write the failing root-free runtime test**

Temporarily add a raw desired trigger builder in the passive CCTP test. Serialize:

```rust
TriggerArgs {
    params,
    paired_adapter_ix_data_hash,
}
```

and omit `address_book_root` from the current account metas. Run:

```bash
NO_DNA=1 anchor build --program-name sra_handler
cargo test -p sra-handler --test passive_source passive_cctp_happy_path -- --nocapture
```

Expected RED: instruction 0 rejects the 17-account layout before adapter execution.

- [ ] **Step 3: Remove dead trigger validation**

In `trigger.rs`:

- Remove the final-handler equality check.
- Remove `find_program_address`, expected-key comparison, and expected-bump comparison.
- Keep one `config_hash` computation for `signer_seeds`.
- Keep active, initiator, positive amount, source balance, stored route, address-book-entry enabled, adapter, fee, slippage, adjacency, nonce, and transfer checks.

- [ ] **Step 4: Remove `address_book_root` from `Trigger`**

Delete the typed root account and the `#[instruction(params: PrepareParams)]` attribute. Derive `address_book_entry` with:

```rust
#[account(
    seeds = [
        SEED_ADDRESS_BOOK,
        sra_config.handler_data.address_book_root.as_ref(),
        source_mint.key().as_ref(),
        &sra_config.handler_data.target_chain_id.to_le_bytes(),
        &sra_config.handler_data.target_token,
    ],
    bump = address_book_entry.bump,
)]
pub address_book_entry: Box<Account<'info, AddressBookEntry>>,
```

The route equality check against `params` remains in the handler.

- [ ] **Step 5: Make `TriggerArgs` the entrypoint contract**

Change both entrypoint and handler to:

```rust
pub fn trigger(ctx: Context<Trigger>, args: TriggerArgs) -> Result<()>
```

Destructure `args` once in the handler. Update every generated builder to:

```rust
sra_handler::instruction::Trigger {
    args: TriggerArgs {
        params,
        paired_adapter_ix_data_hash,
    },
}
```

- [ ] **Step 6: Move optional-program-account decoding**

Create `programs/sra-handler/src/utils/program_account.rs` with the existing generic decoder. Preserve one concise safety comment stating that `Account::try_from` cannot represent a deliberately empty optional PDA. Re-export the function within the handler crate and use it from `trigger.rs`.

- [ ] **Step 7: Shift shared attestation to slot 7**

Set:

```rust
const HANDLER_ACCT_ADAPTER_PROGRAM: usize = 7;
const MIN_HANDLER_ACCOUNTS: usize = 8;
```

Keep slot 2 for `sra_config` and slot 3 for `source_mint`. The unit fixture must contain the same eight-account prefix as production.

- [ ] **Step 8: Simplify test trigger accounts**

Remove `address_book_root` from `TriggerAccounts`, its `derive` arguments, the generated account builder, and all call sites. Delete root-account substitution from the account-substitution matrix; retain address-book-entry substitution and disabled-entry tests.

Replace the temporary raw builder from Step 2 with generated `Trigger` accounts and instruction data.

- [ ] **Step 9: Rebuild and regenerate IDLs**

Run:

```bash
NO_DNA=1 anchor build --program-name sra_handler
NO_DNA=1 anchor build --program-name sra_cctp_adapter
NO_DNA=1 anchor build --program-name sra_across_adapter
cp target/idl/sra_handler.json idl/sra_handler.json
cp target/idl/sra_cctp_adapter.json idl/sra_cctp_adapter.json
cp target/idl/sra_across_adapter.json idl/sra_across_adapter.json
cargo test -p sra-common ix_attestation -- --nocapture
cargo test -p sra-handler --test passive_source -- --nocapture
```

Expected: shared attestation and all passive tests pass with the root-free generated builder.

- [ ] **Step 10: Verify exact generated ABI**

Confirm `idl/sra_handler.json` has one `args` argument of defined type `TriggerArgs`, 17 trigger accounts, no `addressBookRoot`, `adapterProgram` at index 7, and `addressBookEntry` at index 8. Confirm both adapter IDLs remain generated-identical except for shared type reachability.

- [ ] **Step 11: Commit the wire cleanup**

```bash
git add crates/sra-common/src/ix_attestation.rs programs/sra-handler/src programs/sra-handler/tests idl
git commit -m "refactor(handler): simplify passive trigger contract"
```

---

### Task 3: Centralize nonce derivation and remove redundant adapter state

**Files:**
- Modify: `crates/sra-common/src/types/source.rs`
- Modify: `programs/sra-cctp-adapter/src/instructions/execute_deposit.rs`
- Modify: `programs/sra-across-adapter/src/instructions/execute_deposit.rs`
- Modify: `programs/sra-across-adapter/src/utils/source_deposit.rs`
- Modify: `programs/sra-handler/tests/common/mod.rs`
- Modify: `programs/sra-handler/tests/passive_source.rs`
- Modify: generated adapter and handler IDLs if changed by Anchor

**Interfaces:**
- Produces: `SraConfig::last_triggered_nonce() -> Option<u64>`.
- Preserves: adapter-specific `TriggeredNonceUnavailable` errors and nonce fields on both adapter events.

- [ ] **Step 1: Record existing nonce behavior**

Run:

```bash
cargo test -p sra-cctp-adapter triggered_nonce -- --nocapture
cargo test -p sra-across-adapter triggered_nonce -- --nocapture
```

Expected: both duplicated helper suites pass before refactoring.

- [ ] **Step 2: Add the shared nonce method**

Add:

```rust
impl SraConfig {
    pub fn last_triggered_nonce(&self) -> Option<u64> {
        self.deposit_nonce.checked_sub(1)
    }
}
```

Move the zero and positive boundary assertions to `types::source::tests`, using literal expectations `None`, `Some(0)`, and `Some(6)`.

- [ ] **Step 3: Delete adapter-local nonce helpers**

At each call site use:

```rust
let triggered_nonce = ctx
    .accounts
    .sra_config
    .last_triggered_nonce()
    .ok_or(error!(AdapterError::TriggeredNonceUnavailable))?;
```

Use the actual adapter error enum at each site. Remove the duplicated helper functions, helper-only tests, and helper-only `sample_sra_config` fixtures.

- [ ] **Step 4: Remove redundant CCTP decimals validation**

Change the mint constraint to:

```rust
#[account(mut, address = USDC_MINT)]
pub input_mint: Box<Account<'info, Mint>>,
```

In the CCTP integration suite, retain noncanonical-address rejection and delete the forged canonical-address/wrong-decimals branch.

- [ ] **Step 5: Remove the unused Across builder argument**

Change `build_deposit_now_bundle` to remove `_sra_config: &SraConfig`. Update the adapter handler, both unit tests in `utils/source_deposit.rs`, and `execute_deposit_across_ix` in handler test utilities. Remove now-unused imports and sample fixtures.

- [ ] **Step 6: Verify adapter behavior**

Run:

```bash
NO_DNA=1 anchor build --program-name sra_cctp_adapter
NO_DNA=1 anchor build --program-name sra_across_adapter
cargo test -p sra-common types::source -- --nocapture
cargo test -p sra-cctp-adapter
cargo test -p sra-across-adapter
cargo test -p sra-handler --test passive_source passive_cctp -- --nocapture
cargo test -p sra-handler --test passive_source passive_across -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected: all commands pass and neither adapter contains `fn triggered_nonce`, `sample_sra_config`, or an unused SRA argument in the Across bundle builder.

- [ ] **Step 7: Commit adapter cleanup**

```bash
git add crates/sra-common/src/types/source.rs programs/sra-cctp-adapter programs/sra-across-adapter programs/sra-handler/tests idl
git commit -m "refactor(adapters): share passive trigger state"
```

---

### Task 4: Decompose passive source tests and restore client obligations

**Files:**
- Create: `programs/sra-handler/tests/common/passive.rs`
- Create: `programs/sra-handler/tests/manage_sra.rs`
- Create: `programs/sra-handler/tests/passive_trigger.rs`
- Create: `programs/sra-handler/tests/passive_cctp.rs`
- Create: `programs/sra-handler/tests/passive_across.rs`
- Remove: `programs/sra-handler/tests/passive_source.rs`
- Modify: `programs/sra-handler/tests/common/mod.rs`
- Modify: `programs/sra-handler/tests/recover_custody.rs`
- Modify: `programs/sra-handler/tests/source_e2e_surfpool.rs`
- Modify: `programs/sra-handler/tests/source_tx_size.rs`

**Interfaces:**
- Produces: focused integration-test crates and reusable passive helpers under `common::passive`.
- Preserves: all 20 current passive/manage assertions, full rollback snapshots, and the cross-adapter/mint nonce sequence.

- [ ] **Step 1: Add the common passive module**

Declare and re-export `common::passive`. Move these test-only units from `passive_source.rs`:

```text
set_source_balance
PassiveSnapshot
token_balance_or_zero
passive_snapshot
set_sra_config
cctp_pair
across_pair
assert_rejected_unchanged
assert_sra_error
```

Move only code; do not change behavior or add production APIs.

- [ ] **Step 2: Add `PassiveFixture::for_mint`**

Add a test-only constructor that takes an existing fixture, a new source mint, adapter program, and Across state/vault inputs; it derives the SRA vault, adapter authority/custody, owner ATA, and address-book entry while reusing the original owner, initiator, fee admin, immutable handler data, and `sra_config`.

Replace the full struct literals in the cross-adapter nonce and noncanonical-CCTP tests with this constructor.

- [ ] **Step 3: Make canonical CCTP mint creation explicit**

Extract a named test helper that plants a newly initialized legacy SPL mint at `USDC_MINT`. Call it only from CCTP fixture construction. Fresh-mint fixture construction must not inspect `adapter_program` to decide which address to use.

Delete the stale `across_state` doc comment rather than replacing it.

- [ ] **Step 4: Create `manage_sra.rs`**

Move the five management tests and their two instruction helpers. Replace `SourceFixture` with `PassiveFixture`; use `fx.sra_config`, `fx.source_mint`, `fx.accounts.sra_vault`, and the existing payer/owner/initiator fields. Delete `SourceFixture` entirely.

- [ ] **Step 5: Create `passive_trigger.rs`**

Move generic authorization, active/initiator, account substitution, address-book, stored route, adapter policy, amount/nonce, fee, slippage, pairing, and cross-adapter nonce tests.

Split `passive_trigger_rejects_fee_slippage_and_pairing_mismatches_atomically` into:

```text
passive_trigger_rejects_fee_policy_mismatches_atomically
passive_trigger_rejects_slippage_atomically
passive_trigger_rejects_pairing_mismatches_atomically
```

Each test creates its own SVM/fixture and uses `assert_rejected_unchanged` for every case.

- [ ] **Step 6: Create protocol-specific suites**

Move CCTP happy path, finality/quote matrix, noncanonical mint, and event-account reuse to `passive_cctp.rs`. Move Across happy path, timing, delegate, and CPI rollback to `passive_across.rs`.

Keep the wrong-delegate assertion pinned to instruction 1 and `AcrossAdapterError::InvalidDelegateAccount`.

- [ ] **Step 7: Restore the no-ALT packet regression**

Add `build_source_versioned_tx_without_alt` using an empty lookup-table slice. Restore:

```rust
#[test]
fn source_2ix_versioned_tx_exceeds_packet_limit_without_alt() {
    let mut svm = setup_svm();
    let fx = PassiveFixture::new(&mut svm);
    let params = PrepareParams {
        input_amount: INPUT,
        output_amount: u256_from_u64(OUTPUT_WITHIN_SLIPPAGE),
        ..sample_prepare_params(fx.target_token)
    };
    let tx = build_source_versioned_tx_without_alt(&mut svm, &fx, params);
    let size = bincode::serialize(&tx).expect("bincode serialize tx").len();
    assert!(size > PACKET_DATA_SIZE);
}
```

Run the test and record the exact with-ALT and without-ALT sizes.

- [ ] **Step 8: Verify all moved tests are discovered**

Run:

```bash
cargo test -p sra-handler --test manage_sra
cargo test -p sra-handler --test passive_trigger
cargo test -p sra-handler --test passive_cctp
cargo test -p sra-handler --test passive_across
cargo test -p sra-handler --test source_tx_size -- --nocapture
```

Expected: 5 management tests, all 15 prior passive tests plus the two additional split-test entries, and both packet-size tests pass. No `passive_source` test target remains.

- [ ] **Step 9: Check test structure**

Run:

```bash
wc -l programs/sra-handler/tests/manage_sra.rs \
  programs/sra-handler/tests/passive_trigger.rs \
  programs/sra-handler/tests/passive_cctp.rs \
  programs/sra-handler/tests/passive_across.rs \
  programs/sra-handler/tests/common/passive.rs
```

Confirm each top-level test file has one protocol concern and no full `PassiveFixture` struct literal appears outside `common`.

- [ ] **Step 10: Commit test decomposition**

```bash
git add programs/sra-handler/tests
git commit -m "test(handler): split passive source coverage"
```

---

### Task 5: Regenerate, verify, review, and update PR #108

**Files:**
- Verify: all production and test files changed in Tasks 1-4
- Modify: `idl/sra_handler.json`
- Modify: `idl/sra_cctp_adapter.json`
- Modify: `idl/sra_across_adapter.json`
- Update externally: PR #108 body

**Interfaces:**
- Produces: merge-ready PR #108 with review findings addressed and release evidence current.

- [ ] **Step 1: Rebuild every runtime artifact**

```bash
NO_DNA=1 anchor build --program-name sra_handler
NO_DNA=1 anchor build --program-name sra_cctp_adapter
NO_DNA=1 anchor build --program-name sra_across_adapter
NO_DNA=1 anchor build --program-name mock_cctp
NO_DNA=1 anchor build --program-name mock_spoke_pool
NO_DNA=1 anchor build --program-name mock_swap
```

- [ ] **Step 2: Regenerate and compare IDLs**

```bash
cp target/idl/sra_handler.json idl/sra_handler.json
cp target/idl/sra_cctp_adapter.json idl/sra_cctp_adapter.json
cp target/idl/sra_across_adapter.json idl/sra_across_adapter.json
cmp target/idl/sra_handler.json idl/sra_handler.json
cmp target/idl/sra_cctp_adapter.json idl/sra_cctp_adapter.json
cmp target/idl/sra_across_adapter.json idl/sra_across_adapter.json
```

- [ ] **Step 3: Run the complete verification gate**

```bash
cargo test -p sra-common ix_attestation -- --nocapture
cargo test -p sra-handler --test manage_sra
cargo test -p sra-handler --test passive_trigger
cargo test -p sra-handler --test passive_cctp
cargo test -p sra-handler --test passive_across
cargo test -p sra-handler --test source_fee_collection
cargo test -p sra-handler --test source_tx_size -- --nocapture
cargo test -p sra-handler --test source_cu_budget -- --nocapture
cargo test --workspace
cargo fmt --all -- --check
cargo clippy -p sra-common -p sra-handler -p sra-cctp-adapter -p sra-across-adapter --all-targets -- -D warnings
git diff --check
```

- [ ] **Step 4: Audit the requested removals**

Confirm:

```text
No address_book_root field in Trigger or TriggerAccounts
No trigger-time find_program_address or expected-bump check
adapter_program is handler slot 7 everywhere
No adapter-local triggered_nonce helper
No unused SraConfig argument in build_deposit_now_bundle
No forged canonical-USDC decimals test
No passive_source.rs
Both with-ALT and without-ALT size tests exist
```

- [ ] **Step 5: Request a fresh code review**

Review the complete remediation diff against the pasted request-changes verdict and the approved design. Fix every Critical or Important finding and re-run its focused covering tests.

- [ ] **Step 6: Update PR #108**

Update the PR body to call out:

- the fail-closed slippage semantic difference;
- the 17-account root-free trigger and slot-7 adapter pin;
- shared nonce and optional-account helpers;
- the split passive test suites and restored no-ALT measurement;
- that passive SRA vault funds can leave only through `trigger` until Task 3 adds owner recovery.

- [ ] **Step 7: Commit any final generated-only delta**

If Step 2 changed an IDL after the Task 2 or 3 commits, stage only the generated IDLs and commit:

```bash
git add idl/sra_handler.json idl/sra_cctp_adapter.json idl/sra_across_adapter.json
git commit -m "chore(idl): refresh passive trigger interfaces"
```

If there is no generated-only delta, do not create an empty commit.

- [ ] **Step 8: Push without rewriting PR history**

```bash
git push origin codex/passive-deposits-task-2
```

Verify PR #108 remains based on `cctpV2`, is mergeable, and reports the pushed head SHA.
