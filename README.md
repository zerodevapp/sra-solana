# SRA on Solana

Solana programs for **Smart Routing Accounts (SRA)** — deterministic-address smart accounts that route tokens between Solana and EVM chains. This repo is the Solana counterpart to the EVM SRA contracts in [`sra-stx/`](../sra-stx).

> ⚠️ **Status: pre-production / experimental.**
> Both the source and destination flows — including fees and destination action execution — are implemented, but the code is unaudited and still under active development. Treat anything here as testbed code — fine to clone, build, and play with on a local validator or devnet, **not** ready for mainnet, real funds, or third-party integrations. APIs, account layouts, and program IDs will change.

**Auditors:** for a self-contained walkthrough of the whole system — programs, flows, trust model, invariants, and known divergences — start with [`before-audit.md`](./before-audit.md).

---

## SRA on Solana

An SRA is a deterministic PDA whose address commits to its owner, handler, immutable source routing policy, and salt. It:

- Receives tokens passively in handler-owned Solana vaults and lets the configured initiator route them through an allow-listed bridge adapter (Across or CCTP v2).
- Receives bridged-in tokens on Solana via a fill verifier and runs pre-registered actions on the user's behalf. Protocol fees are taken once on the source side, not on arrival.
- Stays bridge-agnostic and target-agnostic at the handler layer — bridge- and EVM-specific encoding lives in adapter programs.

## Architecture

Three core on-chain programs plus a shared crate, plus test-only mocks:

| Crate / Program          | Role                                                                                                   |
| ------------------------ | ------------------------------------------------------------------------------------------------------ |
| `sra-handler`            | Bridge- and target-agnostic. Source: passive-vault policy validation, fee collection, adapter custody handoff, and routing-event emission. Destination: fill finalization, action execution, recovery. Also hosts fee administration. |
| `sra-across-adapter`     | Across-specific. Source: builds the canonical EVM ABI message + CPIs into Across `deposit_now`. Destination: reads `FillStatusAccount` and produces a `VerifiedFill`. |
| `sra-cctp-adapter`       | CCTP v2-specific. Source: validates the CCTP quote/domain/finality contract and burns canonical USDC through `deposit_for_burn`. |
| `sra-common`             | Shared types, trigger/adapter wire contracts, PDA seeds, constants, errors, and the manual ABI encoder. `declare_id!` matches `sra-handler`'s ID so handler-owned `#[account]` types deserialize correctly across programs. |
| `mock-cctp`              | Test-only CCTP v2 stand-in used by the LiteSVM source tests. |
| `mock-spoke-pool`        | Test-only minimal Across SVM SpokePool mock used by the LiteSVM/Surfpool tests. |
| `mock-swap`              | Test-only swap target used to exercise the destination verified-CPI engine. |

A source-side deposit is a 2-IX versioned transaction: handler `trigger` followed by either `execute_deposit_across` or `execute_deposit_cctp`. The configured initiator authorizes `trigger`; the owner does not sign. `trigger` spends tokens already held in the SRA vault, and the paired instructions bind each other through the Instructions sysvar so neither can execute in isolation. The transaction requires an ALT-backed path to stay within Solana's packet limit. The destination-side fill follows the same two-instruction pattern (`verify_across_fill` + `finalize_fill`), after which `execute_actions` runs the route's committed actions.

The source route tuple and adapter allow-list are immutable in `SraConfig.handler_data`; the owner can rotate the initiator or toggle the SRA active state with `manage_sra`. Each successful trigger increments one `deposit_nonce` stored on the SRA, shared across source mints and adapters. The protocol fee is deducted from the gross input, while the handler's decimal-normalized slippage floor is evaluated against that gross input. CCTP additionally checks that the worst-case post-CCTP-fee delivery covers the committed output.

Local program IDs (see `Anchor.toml`):

```
sra_handler         7CChUDvMWCtqWPbtCs74CCAgvuJnyiqpgNYEocNJrno5
sra_across_adapter  8Fb9Mk2PLcpMjBxd8RWa7TUNMDL2GevMuVEzAbK7YGyU
sra_cctp_adapter    2bXAvxF3U4FtWAnJtcL9LDnwM7qnErf3y3m6USLRKpRw
```

## Project layout

```
sra-solana/
├── Anchor.toml              # Anchor workspace + localnet program IDs
├── Cargo.toml               # Rust workspace (programs/* + crates/*)
├── package.json             # TS toolchain (anchor, mocha, prettier)
├── rust-toolchain.toml      # Pinned to Rust 1.93.1
├── crates/
│   └── sra-common/          # Shared types, seeds, ABI encoder, constants
├── programs/
│   ├── sra-handler/         # Handler program + LiteSVM tests under tests/
│   ├── sra-across-adapter/  # Across adapter: source deposit + destination fill verification
│   ├── sra-cctp-adapter/    # CCTP v2 source deposit adapter
│   ├── mock-cctp/           # Test-only CCTP v2 stand-in
│   ├── mock-spoke-pool/     # Test-only Across SpokePool mock
│   └── mock-swap/           # Test-only swap target for the verified-CPI engine
├── tests/                   # Anchor TS smoke tests (mocha + ts-mocha)
├── idl/                     # Generated IDL artifacts
├── migrations/              # Anchor deploy script
└── target/                  # Build output (gitignored)
```

## Prerequisites

- **Rust** 1.93.1 (pinned via `rust-toolchain.toml`)
- **Solana CLI** 3.1.11
- **Anchor** 1.2.0 (`avm install 1.2.0 && avm use 1.2.0`)
- **Yarn** (the Anchor toolchain is configured for yarn, not npm)
- **Node.js** 20.18 or newer for the TS test harness
- **LiteSVM** 0.16.0 (pinned as a Rust development dependency; no separate binary required)

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
rustc --version   # should report 1.93.1
```

## Build

```bash
# Build all programs + generate IDL
NO_DNA=1 anchor build --ignore-keys

# Or just build the Rust workspace (no IDL generation)
cargo build
```

`anchor build` writes program binaries to `target/deploy/` and IDL JSON to `target/idl/`. When the checked-in IDL changes, copy the generated files from `target/idl/` into `idl/`.

## Test

There are three test surfaces:

**1. LiteSVM Rust tests** (in-process, fast — recommended while iterating)

```bash
# All workspace tests
cargo test

# Just the handler program's tests
cargo test -p sra-handler

# Focused passive-source suites
cargo test -p sra-handler --test manage_sra
cargo test -p sra-handler --test passive_trigger
cargo test -p sra-handler --test passive_cctp
cargo test -p sra-handler --test passive_across
```

These exercise instruction handlers against an in-process LiteSVM runtime. No validator required, but `anchor build` must run first because the suite loads `target/deploy/*.so` artifacts directly.

**2. Anchor + mocha smoke tests** (runs against a local validator)

```bash
# After `anchor build`, run the same generated-client gate as CI
NO_DNA=1 anchor test --skip-build --validator legacy
```

The current TS suite (`tests/sra-handler.ts`) is a smoke test that asserts the generated IDL/client surface — most coverage lives in the Rust LiteSVM tests.

**3. Surfpool E2E** (real Across paths, heavier)

```bash
NO_DNA=1 anchor build --ignore-keys
NO_DNA=1 surfpool start --ci --network mainnet --legacy-anchor-compatibility
NO_DNA=1 cargo test -p sra-handler --test source_e2e_surfpool -- --ignored --nocapture
NO_DNA=1 cargo test -p sra-handler --test dest_e2e_surfpool -- --ignored --nocapture
```

These suites load the local `sra-handler` and adapter programs at their declared Anchor IDs, then exercise the existing ALT-backed v0 source transaction and destination fill flow against the real Across `svm-spoke` program/state.

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
- [x] `manage_sra`
- [x] `set_address_book_entry`
- [x] Passive-vault `trigger`
- [x] Across adapter `execute_deposit_across`
- [x] CCTP v2 adapter `execute_deposit_cctp`
- [x] Across adapter `recover_stuck_custody`
- [x] CCTP v2 adapter `recover_stuck_custody`
- [ ] Owner recovery from the source SRA vault (not yet implemented)

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
- Source-vault funds currently leave only through an initiator-authorized `trigger`. Owner recovery from the source SRA vault is not implemented yet; adapter `recover_stuck_custody` only recovers funds that already reached adapter custody.
- No mainnet deployment exists. **Do not** point any production system at these program IDs.
- **Launch checklist — burn upgrade authority.** The programs are designed to be immutable in production, but that cannot be enforced from inside the repo: it is a manual deploy-time step. After the verifiable build is deployed and verified on-chain, finalize each program with `solana program set-upgrade-authority <PROGRAM_ID> --final` and confirm the authority reads `none` before routing value through it.

## Disclaimer

This code has not been audited. It is published in its current form to let internal collaborators and curious external readers experiment locally. Bugs, breaking changes, force-pushes, and rewritten history should all be assumed.
