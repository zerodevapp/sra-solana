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

A source-side deposit is a 2-IX transaction: handler `trigger` followed by either `execute_deposit_across` or `execute_deposit_cctp`. The configured initiator authorizes `trigger`; the owner does not sign. `trigger` spends tokens already held in the SRA vault, and the paired instructions bind each other through the Instructions sysvar so neither can execute in isolation. The destination-side fill follows the same two-instruction pattern (`verify_across_fill` + `finalize_fill`), after which `execute_actions` runs the route's committed actions.

Repository-constructed application and test transactions use native transaction v1 with inline account addresses and explicit `TransactionConfig` compute-unit and loaded-account-data limits. Every supported source and destination shape fits within 4,096 serialized bytes and 64 inline addresses without Address Lookup Tables. The maximum four-`VerifiedCpi` destination envelope remains intentionally invalid and measurable at 6,830 bytes and 139 addresses. Stock Anchor/Solana loader deployment remains the sole repository-controlled legacy transaction exception.

The source route tuple and adapter allow-list are immutable in `SraConfig.handler_data`; the owner can rotate the initiator or toggle the SRA active state with `manage_sra`. Each successful trigger increments one `deposit_nonce` stored on the SRA, shared across source mints and adapters. The handler deducts the protocol fee before evaluating the decimal-normalized slippage floor against the net input. CCTP additionally checks that the worst-case post-CCTP-fee delivery covers the committed output.

Current program identities (see `Anchor.toml`):

```
sra_handler         GqGSLQKCfuz672dmsNuCusgdwzRubm5iFQ6LbGki51Pk
sra_across_adapter  8Fb9Mk2PLcpMjBxd8RWa7TUNMDL2GevMuVEzAbK7YGyU
sra_cctp_adapter    HBfktmY2oiAmP1m6baQAy9ACJ9uKuyTFyJtHdVYRBL4
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
- **Anchor** 1.2.0 (`avm install 1.2.0 --from-source && avm use 1.2.0`)
- **Yarn** (the Anchor toolchain is configured for yarn, not npm)
- **Node.js** 22.12 or newer for the TS test harness
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
NO_DNA=1 anchor build --ignore-keys --arch v0

# Or just build the Rust workspace (no IDL generation)
cargo build
```

The build command above writes program binaries to `target/deploy/` and IDL JSON to `target/idl/`. When the checked-in IDL changes, copy the generated files from `target/idl/` into `idl/`.

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

These exercise instruction handlers against an in-process LiteSVM runtime. Repository-constructed application transactions use native v1; dependency-owned `litesvm-token` fixture APIs and `LiteSVM::airdrop` still use internal legacy setup transactions. No validator required, but the program build must run first because the suite loads `target/deploy/*.so` artifacts directly.

**2. Anchor + mocha smoke tests** (runs against a local validator)

```bash
# After the program build, run the same generated-client gate as CI
NO_DNA=1 anchor test --skip-build --validator legacy
```

The current TS suite (`tests/sra-handler.ts`) is a smoke test that asserts the generated IDL/client surface — most coverage lives in the Rust LiteSVM tests.

**3. Surfpool E2E** (real Across paths, heavier)

```bash
NO_DNA=1 anchor build --ignore-keys --arch v0
NO_DNA=1 surfpool start --ci --network mainnet --legacy-anchor-compatibility
```

After the RPC health check succeeds, run the exact `Install branch programs` stage from the [source workflow](./.github/workflows/source-e2e-surfpool.yml) or [destination workflow](./.github/workflows/dest-e2e-surfpool.yml). That stage clones placeholder program accounts, assigns the local upgrade authority, and deploys `sra-handler` plus both adapters before either suite runs.

```bash
NO_DNA=1 cargo test -p sra-handler --test source_e2e_surfpool -- --ignored --nocapture
NO_DNA=1 cargo test -p sra-handler --test dest_e2e_surfpool -- --ignored --nocapture
```

These suites load the local `sra-handler` and adapter programs at their declared Anchor IDs, then exercise native-v1 source and destination application transactions with inline addresses against the real Across `svm-spoke` program/state. Program deployment continues to use the stock Anchor/Solana loader path.

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
- [x] Owner recovery from the source SRA vault

Destination-side handler:

- [x] `create_route` / `manage_route`
- [x] `finalize_fill`
- [x] `execute_actions` / `recover_funds` / `retry_actions`
- [x] Across adapter `verify_across_fill`
- [x] Across adapter `verify_across_fill_after_close`

Shared:

- [x] `sra-common`: PDA seeds, types, ABI encoder, constants, errors
- [x] Fee administration (`initialize_fee_config`, `set_protocol_fee`, `set_asset_fee_tier`, two-step admin transfer)
- [x] Foundry golden-vector cross-validation suite
- [x] Native-v1 source and destination transactions with inline-address size canaries
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

- The handler and CCTP adapter are deployed to Solana mainnet for controlled canary testing. The Across adapter ID is configured but not deployed. These identities may rotate before an audited production launch.
- The TS test harness is intentionally thin — Rust LiteSVM tests are the source of truth for behavioural coverage today.
- The SRA owner can withdraw source-vault funds through `recover_source_vault`; adapter `recover_stuck_custody` applies after funds reach adapter custody.
- Across fills should be verified before `exclusivity_deadline`; later verification requires exclusivity through `fill_deadline`. After a fill-status account closes, the owner-signed fallback cannot distinguish a closed account from one that never existed, so the route owner must be trusted with the route vault's available liquidity.
- The deployed mainnet programs are unaudited and retain an upgrade authority. **Do not** treat this deployment as production-ready.
- **Launch checklist — burn upgrade authority.** The programs are designed to be immutable in production, but that cannot be enforced from inside the repo: it is a manual deploy-time step. After the verifiable build is deployed and verified on-chain, finalize each program with `solana program set-upgrade-authority <PROGRAM_ID> --final` and confirm the authority reads `none` before routing value through it.

## Disclaimer

This code has not been audited. It is published in its current form to let internal collaborators and curious external readers experiment locally. Bugs, breaking changes, force-pushes, and rewritten history should all be assumed.
