# SRA on Solana

Solana programs for **Smart Routing Accounts (SRA)** — deterministic-address smart accounts that route tokens between Solana and EVM chains. This repo is the Solana counterpart to the EVM SRA contracts in [`sra-stx/`](../sra-stx).

> ⚠️ **Status: pre-production / experimental.**
> The programs are still under active development. Only a subset of the source-side handler instructions are wired up today. Treat anything here as testbed code — fine to clone, build, and play with on a local validator or devnet, **not** ready for mainnet, real funds, or third-party integrations. APIs, account layouts, and program IDs will change.

---

## SRA on Solana

An SRA is a deterministic per-(owner, app, target) PDA that:

- Receives tokens on Solana and routes them to a target chain via a pluggable bridge adapter (Across in v1).
- Receives bridged-in tokens on Solana via a fill verifier, deducts protocol/app fees, and runs pre-registered actions on the user's behalf.
- Stays bridge-agnostic and target-agnostic at the handler layer — bridge- and EVM-specific encoding lives in adapter programs.

## Architecture

Two on-chain programs plus a shared crate:

| Crate / Program          | Role                                                                                                   |
| ------------------------ | ------------------------------------------------------------------------------------------------------ |
| `sra-handler`            | Bridge- and target-agnostic. Source: validation, vault custody, raw routing-event emission. Destination: fill finalization, fee deduction, action execution. |
| `sra-across-adapter`     | Across-specific. Source: builds the canonical EVM ABI message + CPIs into Across `deposit_now`. Destination: reads `FillStatusAccount` and produces a `VerifiedFill`. |
| `sra-common`             | Shared types, PDA seeds, constants, errors, and the manual ABI encoder. `declare_id!` matches `sra-handler`'s ID so handler-owned `#[account]` types deserialize correctly across both programs. |

A source-side deposit ships as a 2-IX versioned transaction (`validate_and_prepare` + `execute_deposit_across`); a destination-side fill is similarly two instructions (`verify_across_fill` + `finalize_fill`). Both instruction pairs are bound together via the Instructions sysvar so neither can execute in isolation.

Local program IDs (see `Anchor.toml`):

```
sra_handler         Gte7LViTnE2UixBF59Xfu2sSqcmHhMNANj42yUkYpkdo
sra_across_adapter  8n167Xs4Xx7zEq5fuyCDVgYd3vUjFmHAkGTdK2SCpmXD
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
│   └── sra-across-adapter/  # Across adapter (currently a stub)
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

`anchor build` writes program binaries to `target/deploy/` and IDL JSON to `target/idl/` (mirrored into `idl/`).

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

These exercise instruction handlers against an in-process LiteSVM runtime. No validator required.

**2. Anchor + mocha smoke tests** (runs against a local validator)

```bash
# Spawns solana-test-validator, deploys, runs tests/*.ts, then tears down
anchor test
```

The current TS suite (`tests/sra-handler.ts`) is a smoke test that asserts the generated IDL/client surface — most coverage lives in the Rust LiteSVM tests.

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

- [x] `initialize_sra`
- [x] `update_adapters`
- [x] `update_slippage`
- [x] `set_address_book_entry`
- [x] `validate_and_prepare`
- [ ] Across adapter `execute_deposit_across`

Destination-side handler:

- [ ] `create_route` / `manage_route`
- [ ] `finalize_fill`
- [ ] `execute_actions` / `recover_funds` / `retry_actions`
- [ ] Across adapter `verify_across_fill`

Shared:

- [x] `sra-common`: PDA seeds, types, ABI encoder, constants, errors
- [ ] `initialize_fee_config` / `register_app_fee`
- [x] Foundry golden-vector cross-validation suite
- [ ] SDK + ALT setup
- [ ] Devnet staging

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
- No mainnet deployment exists. **Do not** point any production system at these program IDs.

## Disclaimer

This code has not been audited. It is published in its current form to let internal collaborators and curious external readers experiment locally. Bugs, breaking changes, force-pushes, and rewritten history should all be assumed.
