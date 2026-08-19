# SRA on Solana

Solana programs for **Smart Routing Accounts (SRA)** — deterministic-address smart accounts that route tokens between Solana and EVM chains. This repo is the Solana counterpart to the EVM SRA contracts in [`sra-stx/`](../sra-stx).

> ⚠️ **Status: pre-production / experimental.**
> Both the source and destination flows — including fees and destination action execution — are implemented, but the code is unaudited and still under active development. Treat anything here as testbed code — fine to clone, build, and play with on a local validator or devnet, **not** ready for mainnet, real funds, or third-party integrations. APIs, account layouts, and program IDs will change.

**Auditors:** for a self-contained walkthrough of the whole system — programs, flows, trust model, invariants, and known divergences — start with [`before-audit.md`](./before-audit.md).

---

## SRA on Solana

An SRA is a deterministic per-(owner, app, target) PDA that:

- Receives tokens on Solana and routes them to a target chain via a pluggable bridge adapter (Across in v1).
- Receives bridged-in tokens on Solana via a fill verifier and runs pre-registered actions on the user's behalf. Protocol fees are taken once on the source side, not on arrival.
- Stays bridge-agnostic and target-agnostic at the handler layer — bridge- and EVM-specific encoding lives in adapter programs.

## Architecture

Two core on-chain programs plus a shared crate, plus test-only mocks:

| Crate / Program          | Role                                                                                                   |
| ------------------------ | ------------------------------------------------------------------------------------------------------ |
| `sra-handler`            | Bridge- and target-agnostic. Source: validation, vault custody handoff, raw routing-event emission. Destination: fill finalization, action execution, recovery. Also hosts fee administration. |
| `sra-across-adapter`     | Across-specific. Source: builds the canonical EVM ABI message + CPIs into Across `deposit_now`. Destination: reads `FillStatusAccount` and produces a `VerifiedFill`. |
| `sra-common`             | Shared types, PDA seeds, constants, errors, and the manual ABI encoder. `declare_id!` matches `sra-handler`'s ID so handler-owned `#[account]` types deserialize correctly across both programs. |
| `mock-spoke-pool`        | Test-only minimal Across SVM SpokePool mock used by the LiteSVM/Surfpool tests. |
| `mock-swap`              | Test-only swap target used to exercise the destination verified-CPI engine. |

A source-side deposit currently ships as a 2-IX versioned transaction (`validate_and_prepare` + `execute_deposit_across`) and now requires an ALT-backed transaction path to stay within Solana's packet limit. The paired source instructions are bound together via the Instructions sysvar so neither can execute in isolation. The destination-side fill follows the same two-instruction pattern (`verify_across_fill` + `finalize_fill`), after which `execute_actions` runs the route's committed actions.

Local program IDs (see `Anchor.toml`):

```
sra_handler         7CChUDvMWCtqWPbtCs74CCAgvuJnyiqpgNYEocNJrno5
sra_across_adapter  8Fb9Mk2PLcpMjBxd8RWa7TUNMDL2GevMuVEzAbK7YGyU
```

## Project layout

```
sra-solana/
├── Anchor.toml              # Anchor workspace + localnet program IDs
├── Cargo.toml               # Rust workspace (programs/* + crates/*)
├── package.json             # TS toolchain (anchor, mocha, prettier)
├── rust-toolchain.toml      # Pinned to Rust 1.89.0
├── crates/
│   └── sra-common/          # Shared types, seeds, ABI encoder, constants
├── programs/
│   ├── sra-handler/         # Handler program + LiteSVM tests under tests/
│   ├── sra-across-adapter/  # Across adapter: source deposit + destination fill verification
│   ├── mock-spoke-pool/     # Test-only Across SpokePool mock
│   └── mock-swap/           # Test-only swap target for the verified-CPI engine
├── tests/                   # Anchor TS smoke tests (mocha + ts-mocha)
├── idl/                     # Generated IDL artifacts
├── migrations/              # Anchor deploy script
└── target/                  # Build output (gitignored)
```

## Prerequisites

- **Rust** 1.89.0 (pinned via `rust-toolchain.toml`)
- **Solana CLI** ≥ 2.3 (matches the `solana-program`/`solana-sdk` versions in `Cargo.toml`)
- **Anchor** 0.32.1 (`avm install 0.32.1 && avm use 0.32.1`)
- **Yarn** (the Anchor toolchain is configured for yarn, not npm)
- **Node.js** ≥ 18 for the TS test harness

Optional but useful:

- A funded `~/.config/solana/id.json` keypair for `solana-test-validator` / devnet usage
- `cargo nextest` for faster Rust test runs

## Setup

```bash
# Install JS deps
yarn install

# Verify your toolchain
solana --version
anchor --version
rustc --version   # should report 1.89.0
```

## Build

```bash
# Build all programs + generate IDL
anchor build

# Or just build the Rust workspace (no IDL generation)
cargo build
```

`anchor build` writes program binaries to `target/deploy/` and IDL JSON to `target/idl/`. When the checked-in IDL changes, copy the generated files from `target/idl/` into `idl/`.

## Test

There are two test surfaces:

**1. LiteSVM Rust tests** (in-process, fast — recommended while iterating)

```bash
# All workspace tests
cargo test

# Just the handler program's tests
cargo test -p sra-handler

# A single test file
cargo test -p sra-handler --test validate_and_prepare
```

These exercise instruction handlers against an in-process LiteSVM runtime. No validator required, but `anchor build` must run first because the suite loads `target/deploy/*.so` artifacts directly.

**2. Anchor + mocha smoke tests** (runs against a local validator)

```bash
# Spawns solana-test-validator, deploys, runs tests/*.ts, then tears down
anchor test
```

The current TS suite (`tests/sra-handler.ts`) is a smoke test that asserts the generated IDL/client surface — most coverage lives in the Rust LiteSVM tests.

**3. Surfpool source E2E** (real Across path, heavier)

```bash
NO_DNA=1 anchor build
NO_DNA=1 surfpool start --ci --network mainnet --legacy-anchor-compatibility
cargo test -p sra-handler --test source_e2e_surfpool -- --ignored --nocapture
```

This uses Surfpool's auto-deploy path to load the local `sra-handler` and `sra-across-adapter` programs at their declared Anchor IDs, then runs the Step 21 source deposit flow against the real Across `svm-spoke` program/state.

## Running on localnet

```bash
# Start a fresh validator with the programs preloaded
anchor localnet

# In another shell, deploy + run any custom scripts
anchor deploy
```

If you want a long-lived validator outside `anchor test`, run `solana-test-validator` directly and point Anchor at it via `Anchor.toml` (`[provider] cluster = "localnet"`).

## Implementation status

Source-side handler:

- [x] `initialize_address_book_root`
- [x] `initialize_sra`
- [x] `set_address_book_entry`
- [x] `validate_and_prepare`
- [x] Across adapter `execute_deposit_across`
- [x] Across adapter `recover_stuck_custody`

Destination-side handler:

- [x] `create_route` / `manage_route`
- [x] `finalize_fill`
- [x] `execute_actions` / `recover_funds` / `retry_actions`
- [x] Across adapter `verify_across_fill`

Shared:

- [x] `sra-common`: PDA seeds, types, ABI encoder, constants, errors
- [x] Fee administration (`initialize_fee_config`, `set_protocol_fee`, `set_asset_fee_tier`, two-step admin transfer)
- [x] Foundry golden-vector cross-validation suite
- [x] ALT-backed source transaction reference path in tests
- [x] Destination E2E against real Across via Surfpool
- [x] Devnet deployment
- [ ] Security audit

## Linting & formatting

```bash
# Rust
cargo fmt --all
cargo clippy --all-targets -- -D warnings

# TS / JS
yarn lint        # check
yarn lint:fix    # rewrite
```

## Known caveats

- Program IDs in `Anchor.toml` and `declare_id!` are dev placeholders; they will rotate before any audited deployment.
- The TS test harness is intentionally thin — Rust LiteSVM tests are the source of truth for behavioural coverage today.
- Across fills should be verified by a permissionless cranker before `exclusivity_deadline`. Later verification is accepted only when the relay's exclusivity lasts through `fill_deadline`; slow fills and other amount-ambiguous fills fail closed.
- No mainnet deployment exists. **Do not** point any production system at these program IDs.
- **Launch checklist — burn upgrade authority.** The programs are designed to be immutable in production, but that cannot be enforced from inside the repo: it is a manual deploy-time step. After the verifiable build is deployed and verified on-chain, finalize each program with `solana program set-upgrade-authority <PROGRAM_ID> --final` and confirm the authority reads `none` before routing value through it.

## Disclaimer

This code has not been audited. It is published in its current form to let internal collaborators and curious external readers experiment locally. Bugs, breaking changes, force-pushes, and rewritten history should all be assumed.
