# Copilot code review — sra-solana

This is a fund-custody cross-chain bridge: two Anchor (Solana/Rust) programs —
`sra-handler` and `sra-across-adapter` — plus the `sra-common` crate and a
`mock-spoke-pool` test double. Review security first and be adversarial.

## How to review
- Prioritize in this order: (1) fund-safety/security, (2) correctness vs the
  cross-chain counterpart, (3) Solana/Anchor footguns, (4) tests that don't
  actually test, (5) maintainability. Never bury a fund-safety issue under style.
- For every issue, give: a severity (Critical/High/Medium/Low), the concrete
  impact ("a relayer can redirect the output to their own account"), the exact
  `file:line`, and a concrete fix. No vague "consider reviewing this."
- Flag uncertainty explicitly. If an invariant can't be verified from the diff
  alone (needs a file not shown, the on-chain counterpart, or author intent),
  say so and ask a direct question — don't assume it's fine and don't assume
  it's broken. Label these "Unverified — needs author confirmation."
- Calibrate noise to severity: for security/fund-safety, raise a concern even
  when you can't fully confirm it (mark it "Unverified") — a missed exploit
  costs more than a false alarm. For non-security findings, comment only when
  you're confident a real issue exists; if it's a "maybe," stay silent. Noise
  erodes trust in the review.
- Favor consistency with this repo: prefer existing helpers/patterns over new
  one-offs, flag logic placed in the wrong layer, and use the PR description for
  intent — call out changes unrelated to its stated purpose (scope creep) or a
  behavior/interface change the description doesn't mention.
- `idl/` and `idls/` are generated/vendored — don't nitpick their formatting.

## Security — Solana/Anchor program safety (highest priority)
Assume the attacker controls every account passed in, every instruction
argument, transaction ordering, and the CPI call graph.
- Missing owner/signer checks: an account trusted without verifying its owning
  program (`Account<'info,T>` or explicit `.owner`), or an authority used
  without `Signer<'info>`/`is_signer`.
- Account substitution: a value-routing account (vault, recipient ATA, fee
  account, authority) that is caller-chosen instead of bound by `seeds` /
  `associated_token::` constraints to committed state. The fill path has no
  owner signature — anything security-relevant must be committed at create time
  and equality-checked per fill; a relayer must never redirect input or output.
- Missing payer/authority binding: a guard that matches an account by address
  or amount but ignores *who* funded/authorized it (any actor could satisfy it).
- Duplicate mutable accounts: the same account passed into two mutable slots, so
  one write silently overwrites the other or a "transfer" becomes a self-transfer.
  Mutable accounts that must differ have to be checked for inequality.
- Data matching: a stored field (owner/authority/mint/route) must be checked
  against the account actually passed or signing (e.g. `has_one`) — type/owner
  checks alone don't prove the right *instance* was supplied.
- Arbitrary CPI: `invoke`/`invoke_signed` into a program id that isn't pinned
  (exact pubkey + loader-owned-executable). For PDA-signed CPIs, the input must
  be bounded (move only the committed fraction into a scratch account; never
  expose the full vault), and source/destination/authority slots must be
  handler-pinned, not relayer-supplied.
- PDA hazards: non-canonical bump, PDA/seed reuse across roles, seed collisions.
- Account lifecycle: reinitialization (`init_if_needed` without a re-init
  guard), unsafe close (data not zeroed / lamports not fully drained); replay
  or receipt markers must never be closeable.
- Type/layout: deserializing without a discriminator/owner check, or accepting
  trailing bytes. Require exact layout; reject trailing bytes.
- `remaining_accounts` bypass Anchor validation and must be validated manually
  (pubkey, order, writability, owner). Cross-instruction attestation must pin
  program id + account slots + hash the exact instruction data.
- Sysvars (clock / rent / instructions): read via checked accessors or a pinned
  canonical address — never a caller-passed account trusted by its layout alone.
- Token-2022: use the post-fee received balance (amount received != amount
  sent), pass through transfer-hook extra accounts and budget CU, and never
  silently custody permanent-delegate / pausable / default-frozen mints (require
  explicit opt-in). Don't assume SPL-Token semantics.

## Arithmetic & cross-chain value
- All token-amount math in `u128` with `checked_*` (or a justified
  saturating/`div_ceil`); flag unchecked `+ - *` on value and silent `as` casts.
- Rounding direction must match the counterpart contract: a minimum/slippage
  floor the reference ceils must ceil here too. Flag floor-vs-ceil divergence.
- Foreign (EVM-origin) `[u8;32]` amounts narrowed to `u64`/`u128` must check the
  high bytes are zero with a dedicated error — never silently truncate.
- Flag a strict `==` on a value that can legitimately vary (e.g. a size that
  differs by token), which silently never matches — prefer `>=`/range + explain.
- Flag a guard that silently passes when an optional field is `None` (fail-open).
  Security guards must fail closed.

## Correctness vs the counterpart
- This program mirrors the EVM contracts in the companion `zerodevapp/sra-stx`
  repo (the parity source of truth — `BaseHandler.sol` / `SRAHandler.sol`).
- Flag logic that diverges from parity (fee/slippage formulas, resolution order,
  replay-key construction, message encoding) unless the divergence is explained
  in the diff. If you can't confirm parity from this PR alone, flag it as
  unverified, name the counterpart file to check, and ask.

## Atomicity & state
- Flag partial state mutation on an error/early-return path (e.g. a vector or
  account mutated in a loop that then bails, leaving half-applied state). On
  rejection, state must be unchanged.
- Flag redundant per-iteration external reads (RPC/sysvar/account loads in a
  loop) and inconsistent snapshots (re-reading clock/slot/price per item so
  items disagree) — read once, reuse.

## Anchor / Rust hygiene
- A `match` on an enum that gained a variant must stay exhaustive (no catch-all
  that silently drops a case).
- Account sizing is `space = 8 + T::INIT_SPACE` (the 8-byte discriminator is not
  auto-added); flag bare `INIT_SPACE`.
- Box large `Account<'info,T>`; put fail-fast bound checks first.
- New error-enum variants must be appended at the tail (stable codes); flag
  reordering or insertion among existing variants.
- Hardcoded constants (sizes, fees, program ids, discriminators) must match the
  real on-chain/source value; flag a magic number that may not.

## Tests
- Flag tautological/no-op tests: `assert_eq!(x, x)`, asserting a value against
  itself, or a test that would still pass if the code under test were a no-op.
- Each new instruction/validation needs a happy path, a per-field rejection test
  (asserting the exact error code AND that balances/state are untouched), and
  event/post-state assertions. Flag a rejection test that doesn't pin the code.
- You don't run the tests — review them as code.

## Repo conventions
- This repo is public-facing: flag any internal planning/audit references in
  code, comments, doc-comments, or the IDL (step labels, audit-finding codes,
  internal issue/PR numbers, internal-doc pointers, process jargon). Keep the
  technical substance, drop the citation.
- If a change modifies the public interface (instruction args/accounts, account structs, events, or types) in `sra-handler` or
  `sra-across-adapter`, the committed `idl/*.json` must be regenerated in the
  same PR; flag such a change with no matching IDL update.

## Don't flag (avoid noise)
- CI already runs `cargo fmt`, `cargo clippy -D warnings`, the build, and the
  full test suite — don't report formatting, lint-level, compile, or
  "missing test" issues those gates already catch.
- Don't leave comments that just restate what the code plainly does, or
  pedantic naming/wording nits.
- Don't propose refactors, extra logging, or doc tweaks unless they fix a real
  bug or enforce a convention stated here.
- Prefer one high-value comment over several minor ones.
