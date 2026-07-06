# SRA on Solana — Before You Audit

This document orients a security auditor to the `sra-solana` codebase. It describes what the system does, how the on-chain programs are structured, the trust model, the invariants the design leans on, and where to focus review effort. It is self-contained: you should not need any external design docs to start.

> The repository `README.md` is a build/run guide and its "Implementation status" checklist is **stale** — it lists the destination side as unbuilt. That is no longer true. Trust *this* document and the source tree for scope. The full source and destination lifecycles, fees, and the action-execution engine are all implemented.

---

## 1. What the system is

An **SRA** is a deterministic, address-committed smart account that moves tokens between Solana and EVM chains over an untrusted bridge (Across in v1) and, on arrival, runs a fixed set of pre-committed actions.

The defining property: an SRA's routing policy (and, on the destination side, its action list) is **hashed into the account's PDA address at creation**. Because the address commits to the policy, no relayer, keeper, or bridge can redirect funds or alter the action list at runtime — doing so would require a different address that the user never funded.

There are two independent flows:

- **Source (Solana → EVM):** a keeper submits a two-instruction transaction that validates the routing policy, collects the protocol fee, hands net tokens to the Across adapter, and deposits into Across. The EVM side then fills and executes — the EVM `sra-stx` contracts are unchanged by this repo.
- **Destination (EVM → Solana):** Across fills tokens into a route's vault; an adapter attests the fill on-chain, the handler settles it, and an optional action engine runs the committed actions (transfers / verified swaps). Any non-terminal state has a recovery path.

This repo is the Solana counterpart to the EVM SRA contracts in the sibling `sra-stx/` tree. The EVM contracts require no changes to interoperate.

---

## 2. Programs, crates, and audit scope

| Unit | Kind | In audit scope? | Responsibility |
| --- | --- | --- | --- |
| `programs/sra-handler` | On-chain program | **Yes — primary** | Bridge- and target-agnostic. Source validation + fee + custody handoff + routing event; destination fill settlement, action execution, recovery; fee administration. |
| `programs/sra-across-adapter` | On-chain program | **Yes — primary** | Across-specific. Source: build the canonical EVM ABI message and CPI into Across `deposit_now`. Destination: read the Across fill-status account and mint a `VerifiedFill` attestation. |
| `crates/sra-common` | Shared library crate | **Yes** | PDA seeds, shared account types, constants, errors, the manual ABI encoder, fee math, and the signed-quote message layout. Compiled into both programs. |
| `programs/mock-spoke-pool` | Test-only program | **No — out of scope** | Minimal Across SVM SpokePool stand-in used by LiteSVM/Surfpool tests. |
| `programs/mock-swap` | Test-only program | **No — out of scope** | Generic swap target used to exercise the verified-CPI engine in tests. |

`sra-common`'s `declare_id!` is intentionally set to **the handler's program ID** (`Gte7…pkdo`). This is not a bug: the handler-owned `#[account]` types live in `sra-common`, and Anchor's owner check derives from `declare_id!`, so both crate and program must agree on the owning program. The adapter defines its own ID.

**Program IDs (current dev/devnet values — will rotate before any audited or mainnet deployment; do not treat as final):**

```
sra_handler         Gte7LViTnE2UixBF59Xfu2sSqcmHhMNANj42yUkYpkdo
sra_across_adapter  9xsKH8vdhYbNy9hYEp4e9svbMRjGWpBo2Zbt8NWFnj2D
```

**The deployed handler program is intended to be immutable (upgrade authority burned) at launch.** This is why source and destination instructions live in a single binary — no new instructions can be added after freeze. Review with the assumption that nothing here is patchable post-deploy.

---

## 3. End-to-end flows

### 3.1 Source: Solana → EVM

One atomic transaction, two paired instructions (an Address Lookup Table is **mandatory** — the transaction exceeds the 1232-byte packet limit without one; CI machine-checks this):

1. **`validate_and_prepare` (handler)**
   - Recompute the committed `handler_data` hash and check it equals the value stored in the SRA config; check `final_handler == handler program ID`.
   - Verify the chosen adapter is in `allowed_adapters` and the address-book entry for `(source_mint, target_chain_id, target_token)` is enabled.
   - Recompute the protocol fee and assert it equals the caller-attested amount.
   - Enforce the slippage floor on the **post-fee** amount.
   - Transfer owner ATA → SRA vault (owner signs), then SRA vault → adapter custody (SRA PDA signs).
   - Advance the per-`(SRA, mint)` nonce and emit the routing event.
   - **Pin the next instruction:** via the Instructions sysvar, assert the following instruction is the paired `execute_deposit_across` for the same config/amounts (data-hash bound). Neither instruction can execute alone.

2. **`execute_deposit_across` (adapter)**
   - **Pin the previous instruction** back to `validate_and_prepare` (same mutual attestation).
   - Narrow the bytes32 EVM routing fields to 20-byte EVM addresses, build the canonical ABI message, derive the Across delegate PDA, approve custody, and CPI Across `deposit_now`.
   - Enforce Across timing floors (`fill_deadline_offset ≥ 300s`; `exclusivity_period ≤ fill_deadline_offset`).

Result: tokens enter Across; a relayer fills on the EVM target; the EVM handler executes. Nothing further happens on Solana for a source deposit.

### 3.2 Destination: EVM → Solana

**Setup (owner-gated, once per route):**

- `create_route` — commit the immutable route identity (owner, source chain, expected mint, **action config**, and the initial adapter/executor allowlists), all folded into the route PDA's `route_config_hash` seed; initialize the route vault and execution vault ATAs and their authority PDAs.
- `manage_route` — mutate only the *policy* fields: `paused`, `allowed_adapters`, `allowed_executors`. The action config and identity fields are immutable.

**Fill lifecycle (deterministic; parts are permissionless):**

1. Across fills output tokens into the route vault ATA (external, untrusted).
2. **`verify_across_fill` (adapter, permissionless)** — read the Across `FillStatusAccount` at its pinned PDA, confirm `Filled`, re-derive the relay hash from supplied relay data (cryptographic binding to the fill), cross-check recipient = route / output token = expected mint / origin chain = source chain, and mint a `VerifiedFill` PDA (`init` → replay-once).
3. **`finalize_fill` (handler, permissionless)** — require `!paused`; require the `VerifiedFill`'s owning program is in `allowed_adapters`; deserialize and re-validate every field; move the full verified amount route vault → execution vault (execution authority PDA signs); `init` a `FillReceipt` replay marker with status `Finalized`. **No fee is taken here** — fees are source-side only.
4. **`execute_actions` (handler, permissionless but executor-gated)** — require an executor in `allowed_executors` and `!paused`; run the committed `action_config` (hash-verified against the route). Two-pass: structural problems (frozen ATA, missing account) soft-fail to `ActionsFailed`; a CPI revert or `min_out` shortfall hard-fails the whole transaction. Success → `ActionsSucceeded`.

**State machine (`ExecutionStatus` on `FillReceipt`):**

```
Finalized ──execute_actions──▶ ActionsSucceeded   (terminal)
    │                └────────▶ ActionsFailed
    │                                │
    │                    ┌───────────┴───────────┐
    │            retry_actions              recover_funds
    │            (owner)                    (owner)
    │                │                          │
    │                ▼                          ▼
    │          ActionsSucceeded            Recovered (terminal)
    │
    └──recover_funds (owner)──▶ Recovered (terminal)
```

Every non-terminal state has an owner-driven exit; the `FillReceipt` is never closeable, so a fill can never be replayed.

---

## 4. Instruction inventory and authorization

The authorization column is the single most important thing to verify against the code. "Permissionless" means anyone can submit but the effect is fully constrained by on-chain checks.

### 4.1 Source (handler)

| Instruction | Who may call | Enforced checks |
| --- | --- | --- |
| `initialize_address_book_root` | Owner (signs) | Owner recorded as root owner (immutable). |
| `set_address_book_entry` | Owner (signs); payer may differ | `root.owner == owner`; `target_token != [0;32]`. |
| `initialize_sra` | Owner (signs) | `handler_data.hash() == config_hash`; owner immutable. |
| `validate_and_prepare` | Owner (signs to authorize token spend); separate fee payer | config-hash match; `final_handler == program ID`; adapter allow-listed; address-book entry enabled; recomputed fee == attested fee; slippage floor on post-fee amount; next-instruction pin. |

### 4.2 Destination (handler)

| Instruction | Who may call | Enforced checks |
| --- | --- | --- |
| `create_route` | Owner (signs) | `route_config_hash` recomputed and matched; action config validated (bounds, SplTransfer bps sum, VerifiedCpi structure); adapter/executor lists distinct and bounded. |
| `manage_route` | Owner (signs, `has_one`) | Mutates only `paused` / adapters / executors. |
| `finalize_fill` | **Permissionless** (caller pays rent) | `!paused`; `verified_fill` owner ∈ `allowed_adapters`; PDA pin `["verified_fill", adapter, fill_id]`; full field re-validation; `FillReceipt` `init` (replay-once). |
| `execute_actions` | **Permissionless**, but executor must be in `allowed_executors` | `!paused`; receipt status `Finalized`; per-action account/ATA pinning; VerifiedCpi program loader-owned + executable + pinned; signed-quote verification. |
| `retry_actions` | Owner (signs, `has_one`) | Receipt status `ActionsFailed`; identical execution path to `execute_actions`. |
| `recover_funds` | Owner (signs, `has_one`) | Receipt status ∈ {`Finalized`, `ActionsFailed`}; drains execution vault → owner ATA; status → `Recovered`. |

### 4.3 Fee administration (handler, shared)

All gated by the `FeeConfig` **admin** (single key, transferable via a two-step `set_pending_admin` → `accept_admin`).

| Instruction | Effect |
| --- | --- |
| `initialize_fee_config` | One-time create of the global `FeeConfig` PDA with a fee recipient. `Pubkey::default()` recipient disables all fees. |
| `set_fee_recipient` | Change recipient (default = fees off globally). |
| `set_protocol_fee` / `remove_protocol_fee` | Create/close the single global `ProtocolFeeConfig` (`default_fee_bps ≤ 1000`, `allow_override` flag). |
| `set_asset_fee_tier` / `remove_asset_fee_tier` | Create/close a per-mint tier `(threshold, below_bps, above_or_equal_bps)`, each ≤ 1000 bps. |
| `set_pending_admin` / `accept_admin` | Two-step admin handoff. |

### 4.4 Adapter (`sra-across-adapter`)

| Instruction | Who may call | Purpose / checks |
| --- | --- | --- |
| `execute_deposit_across` | Bound to the paired handler instruction | Mutual instruction attestation; Across timing floors; approve custody; CPI Across `deposit_now`. |
| `recover_stuck_custody` | SRA owner (signs) | Refund a stranded custody ATA to the owner after a deposit abort; `sra_config.owner == owner`. |
| `verify_across_fill` | **Permissionless** | Read Across fill status (must be `Filled`); re-derive relay hash; cross-check all routing fields and chain ID; mint `VerifiedFill` PDA. |

---

## 5. Account and PDA model

Every stateful account below is a PDA owned by the handler program unless noted. Token vaults are Associated Token Accounts whose *authority* is a handler PDA.

### 5.1 Source-side (handler-owned)

| Account | Seeds | Mutated by | Notes |
| --- | --- | --- | --- |
| `SraConfig` | `["sra", owner, config_hash, salt]` | `initialize_sra` only | Immutable: owner, `final_handler`, `handler_data_hash`, salt. |
| Address book root | owner-scoped root account | `initialize_address_book_root` (owner set once) | Namespace for entries. |
| `AddressBookEntry` | `["address_book", root, source_mint, target_chain_id, target_token]` | `set_address_book_entry` | Enable/disable + `target_decimals`. |
| Mint nonce | `["mint_nonce", sra_config, source_mint]` | `validate_and_prepare` | Per-`(SRA, mint)` uniqueness counter; never closed. |

### 5.2 Destination-side (handler-owned)

| Account | Seeds | Mutated by | Notes |
| --- | --- | --- | --- |
| `SolanaSraRoute` | `["sra_route", owner, route_config_hash, salt]` | `manage_route` (policy fields only) | Immutable identity incl. `expected_mint`, `source_chain_id`, `action_config`; mutable `paused` / adapters / executors. |
| `FillReceipt` | `["fill_receipt", route, fill_id]` | finalize / execute / retry / recover | Lifecycle status; `init` → one per fill; never closeable (replay guard). |
| Route vault (ATA) | ATA of route PDA | Across fill (in), `finalize_fill` (out) | Receives the raw Across fill. |
| Execution vault (ATA) | ATA under `["execution", route]` authority | finalize (in), actions/recover (out) | Holds settled funds during action execution. |
| Scratch authority | `["scratch", route]` | verified-CPI engine | Temporary, bounded holder for verified-CPI inputs; **distinct** from the execution authority so a hostile inner program can never reach the vault. |

There are **three distinct signing authorities** on the destination side — the route PDA (route vault), the execution authority (execution vault), and the scratch authority (verified-CPI input). The separation is load-bearing; see §7.

### 5.3 Adapter-owned

| Account | Seeds | Notes |
| --- | --- | --- |
| Adapter authority | `["adapter_authority", sra_config]` | Signs custody token moves; not a Borsh account. |
| Adapter custody (ATA) | ATA under adapter authority | Holds input tokens mid-deposit. |
| `VerifiedFill` | `["verified_fill", adapter_program_id, fill_id]` | Adapter-minted fill attestation. **The handler reads it as raw bytes** (discriminator + exact layout check), not via an Anchor import, so multiple adapter programs can produce compatible attestations. |

---

## 6. Trust model and actors

| Actor | Trust | Capability |
| --- | --- | --- |
| **Owner** | Trusted (per their own route) | Creates SRA/route; pause/unpause; replace adapter/executor lists; retry; recover. |
| **Keeper / initiator (source)** | Semi-trusted | Chooses adapter + amounts and submits the source tx, but cannot alter the route destination. |
| **Relayer / filler / executor** | **Untrusted** | Fills via Across, supplies routing accounts for verified CPIs, submits `verify`/`finalize`/`execute`. Constrained entirely by on-chain checks. |
| **Quoter** | Trusted per-route (address committed in the action) | Signs a `min_amount_out` bound for each verified-CPI action; only the committed quoter's signature is accepted. |
| **Adapter program** | Trusted per-route (must be in `allowed_adapters`) | Mints the `VerifiedFill` proof. A compromised **allow-listed** adapter can lie about amount/mint/chain — this is the sharpest trust edge. |
| **Across SpokePool** | Trusted, pinned by constant | Custodies bridged funds; fills routes. |
| **Inner target programs** (swaps, etc.) | **Untrusted** | Run via scratch-bounded CPI with a pinned destination and a balance-diff output floor. |

**Assets at risk:** the full fill amount (in route/execution vaults) and any action input. The most consequential compromise is an allow-listed adapter, followed by a dishonest committed quoter, followed by weaknesses in verified-CPI account pinning.

---

## 7. Load-bearing invariants

These are the properties the security of the destination side rests on. Each is a good audit target — try to break it.

1. **Fill authenticity.** A `VerifiedFill` can only correspond to a real Across fill: the `FillStatusAccount` is pinned to the canonical SpokePool, and the relay data is byte-bound to the fill's relay hash.
2. **Per-route adapter trust.** Only attestations owned by a program in `allowed_adapters` are honored; any other `VerifiedFill` is inert.
3. **Replay-once at two layers.** `VerifiedFill` (`init` per adapter+relay_hash) and `FillReceipt` (`init` per route+fill_id, never closeable).
4. **Custody moves are PDA-signed.** Every token move is signed by a handler-controlled PDA (route / execution authority / scratch authority). Relayers supply accounts, never signing authority.
5. **Action commitment.** Executed actions are *exactly* those committed at `create_route` (hash-checked). A relayer cannot inject, drop, or reorder.
6. **Redirect resistance.** Static transfers must exact-match `ATA(recipient, expected_mint)`. Verified-CPI outputs are pinned to the owner's ATA.
7. **CPI containment.** Verified-CPI input is bounded to the scratch account (`input_bps` of the vault), the inner CPI is signed only by the scratch authority (never the execution authority), the output is pinned to the owner's ATA, `min_out` is enforced by post-fee balance diff, and the scratch is swept empty afterward.
8. **Quote integrity.** `min_out` is quoter-signed over a context hash (route ‖ fill_id ‖ action_index), the chain ID, and an expiry — no cross- fill, cross-action, or cross-chain replay.
9. **No-strand guarantee.** Every non-terminal state has an exit path; funds are never permanently locked by a failed action.

---

## 8. Cross-chain encoding

- **Chain ID.** `SOLANA_CHAIN_ID = 34_268_394_551_451` (Across's SVM chain identifier). It is bound into every relay hash and every signed quote. It must match the Across SVM SpokePool's configured chain ID. (An earlier incorrect value was fixed; confirm the constant, not any external doc.)
- **ABI encoder** (`sra-common/src/abi.rs`). A minimal, hand-rolled encoder that produces big-endian 32-byte EVM words. On the source side the adapter uses it to build the canonical message the EVM handler expects; the Across deposit itself carries an **empty message envelope** (the destination executes via its own instructions, not an EVM callback).
- **Across SpokePool** is pinned by `ACROSS_SPOKE_POOL_ID` (`DLv3NggMiSaef97YCkew5xKUHDh13tVGZ7tydt3ZeAru`).

---

## 9. Fees and slippage

- **Where:** the protocol fee is taken **once, on the source side** in `validate_and_prepare`. The destination forwards the full verified amount — there is no destination fee.
- **Resolution order (short-circuits in this order):**
  1. sponsor flag → fee 0 (returns before any bounds check);
  2. fee recipient == `Pubkey::default()` → fee 0 (globally disabled);
  3. no `ProtocolFeeConfig` present → fee 0;
  4. otherwise: per-call override (if `allow_override`) → per-mint asset tier → default bps.
- **Math:** `fee = floor(amount * bps / 10_000)` via a u128 intermediate. `MAX_FEE_BPS = 1000` (10%) is enforced at config-set time.
- **Slippage floor** is enforced on the **post-fee** amount, with decimal-aware rounding, in `validate_and_prepare`. `MAX_SLIPPAGE_BPS = 10_000` (100%, no effective cap — matches EVM).

Because the sponsor and disabled-fee paths short-circuit *before* the override bounds check, a sponsored deposit can never trip a "fee too high" error — this mirrors the EVM ordering deliberately.

---

## 10. Quoter / signed-quote co-signing (destination actions)

Verified-CPI actions do not trust the relayer's claimed output. Instead the committed **quoter** signs a fixed-layout message binding:

```
context_hash = keccak256(route ‖ fill_id ‖ action_index)
+ input_mint, output_mint, input_amount, min_amount_out, expiry, SOLANA_CHAIN_ID
```

The engine locates the ed25519 signature via the Instructions sysvar, verifies it against the route-committed quoter pubkey, and reconstructs the message from the on-chain action context — so a quote is valid for exactly one (route, fill, action) and cannot be replayed. The quote is effectively a transaction co-signature: it must be produced inside the blockhash window, not held until expiry. **There is no on-chain oracle fallback** — an expired or absent quote hard-rejects the action.

---

## 11. External dependencies / CPIs

| Program | Used by | For |
| --- | --- | --- |
| SPL Token / Token-2022 (via `token_interface`) | handler + adapter | `transfer_checked` on all custody moves; Token-2022 supports permissioned/hooked output mints. |
| Associated Token Program | handler + adapter | Vault/recipient/custody ATA init. |
| System Program | all | Account allocation. |
| Across SVM SpokePool | adapter | Source: CPI `deposit_now`. Destination: read the pinned `FillStatusAccount` (no CPI). |
| Untrusted target program | handler (verified-CPI engine) | Dynamic action CPI, contained per §7. |
| Instructions sysvar | handler + adapter | Paired source/adapter instruction attestation; ed25519 quote location. |

---

## 12. Known caveats and open EVM-parity divergences

Flagged proactively so they are not mistaken for findings. Some are deliberate Solana-vs-EVM differences; a few are tracked gaps for a future re-sync.

**Deliberate divergences (intended):**

- **Empty bridge envelope.** No SRA routing metadata travels in the Across packet; the destination reads routing from its own route PDA.
- **No `app_id` / app-keyed fees.** Fees are a global `ProtocolFeeConfig` plus optional per-mint tiers; there is no per-app fee attribution.
- **Permissionless-but-allowlisted destination execution.** EVM gates `executeOnTarget` on the initiator; Solana makes `execute_actions` permissionless but requires an executor in the route's `allowed_executors`.
- **Fail-closed CPI.** Solana cannot try/catch a mid-CPI revert, so an under-`min_out` swap reverts the whole transaction rather than soft-failing — strictly more conservative than EVM.
- **Fail-closed vault shortfall.** If the route vault is short of the verified amount, `finalize_fill` aborts and the fill stays recoverable.

**Tracked gaps / trust assumptions (worth an auditor's eye):**

- **Sponsor gating.** Solana honors the `is_sponsor` flag unconditionally; the EVM side gates sponsorship on an `allowSponsored` policy. This is a known, un-reconciled divergence — confirm the intended behavior before relying on it.
- **No Across integrator/referral ID** on the source deposit (EVM appends one).
- **No oracle fallback** for `min_out` (signed quote is the only path).
- **Shared execution vault per route.** Concurrent in-flight fills of the same route share one execution vault; recovery always drains the full balance safely, but per-fill accounting can be ambiguous if two fills overlap. The design assumes single-tenant routes (owner == route controller); a pooled/ multi-tenant vault would make the generic verified-CPI action unsafe.
- **Permissioned Token-2022 output** (permanent-delegate / pausable / default-frozen) can be frozen or clawed back by its issuer; routes must opt in explicitly to accept such mints.
- **CPI depth.** Solana's ~4–5 level CPI stack is a hard ceiling; deep swap routes plus token-hook accounts can exhaust it.
- **Closed fill-status account.** If Across lets a relayer close the `FillStatusAccount` after the deadline, on-chain fill verification is no longer possible; a signed relay-attestation fallback is designed but not shipped.
- **ALT-mandatory source tx.** The source transaction requires an Address Lookup Table to fit the packet size (CI-enforced).

---

## 13. Building, testing, reproducing

Toolchain: Rust 1.89.0 (pinned), Solana CLI ≥ 2.3, Anchor 0.32.1, Yarn, Node ≥ 18.

```bash
yarn install
anchor build            # builds all programs + generates IDL into target/idl
```

Three test surfaces:

```bash
# 1. LiteSVM Rust tests (in-process, primary behavioural coverage).
#    Requires `anchor build` first — the suite loads target/deploy/*.so.
cargo test                       # whole workspace
cargo test -p sra-handler        # handler only

# 2. Anchor + mocha smoke test (asserts the generated client surface).
anchor test

# 3. Surfpool E2E against the real Across svm-spoke (heavier).
NO_DNA=1 anchor build
NO_DNA=1 surfpool start --ci --network mainnet --legacy-anchor-compatibility
cargo test -p sra-handler --test source_e2e_surfpool -- --ignored --nocapture
```

The Rust LiteSVM tests are the source of truth for behaviour; the TS suite is a thin client-surface smoke test. A Foundry golden-vector suite cross-validates the ABI encoding against the EVM implementation.

---

## 14. Suggested audit focus, in priority order

1. **`VerifiedFill` production and consumption.** `verify_across_fill` in the adapter (relay-hash re-derivation, fill-status pinning, field cross-checks) and `finalize_fill`'s raw-bytes deserialization + adapter-allowlist + full-field re-validation. This is the money boundary.
2. **The verified-CPI engine** (`execute_actions` / `verified_engine`): scratch bounding, authority separation, destination pinning, `min_out` balance-diff, account-template resolution of Free vs Fixed slots, and signed-quote verification. Try to smuggle value out via a hostile account set.
3. **Replay and state machine.** `FillReceipt` transitions, `init`-not- `init_if_needed`, the no-strand exit paths, and the pause gate coverage.
4. **Paired-instruction attestation** on the source side — can either half run in isolation, or with a substituted counterpart?
5. **Fee resolution ordering** and the sponsor short-circuit; and the **slippage floor** decimal-normalization math.
6. **PDA seed derivations** in `sra-common/src/seeds.rs` — confirm every `#[account(seeds = …)]` matches, and that the config/route hash preimages cover all identity fields.
