# Passive Trigger Review Remediation

**Date:** 2026-09-11
**Status:** Approved direction
**PR:** #108

## Goal

Remove redundant trigger work and duplicated helpers, make the handler-to-adapter wire contract single-source, and split passive source tests by concern without changing intended source-deposit behavior or adapter event schemas.

## Trigger account contract

Remove `address_book_root` from `Trigger`. The address-book entry PDA derives from the immutable values stored in `SraConfig.handler_data`:

```text
["address_book", handler_data.address_book_root, source_mint,
 handler_data.target_chain_id, handler_data.target_token]
```

The trigger account order becomes:

```text
0  fee_payer
1  initiator
2  sra_config
3  source_mint
4  sra_vault
5  adapter_authority
6  adapter_custody
7  adapter_program
8  address_book_entry
9  fee_config
10 protocol_fee_config
11 asset_fee_tier
12 fee_recipient_token_account
13 token_program
14 associated_token_program
15 system_program
16 instructions_sysvar
```

Both adapter attestations pin `adapter_program` at slot 7. The stored route equality check remains because adapter parameters are initiator supplied and attested.

## Trigger state validation

Remove the trigger-time SRA PDA search, key comparison, bump comparison, and duplicate final-handler comparison. `Account<SraConfig>` proves program ownership, `initialize_sra` is the only constructor, and SPL transfers signed with the stored seeds fail if the stored identity does not derive the supplied account. The handler still computes `config_hash` once for signer seeds.

The public entrypoint accepts `TriggerArgs` as its single argument. The handler and attestation decoder therefore share the same serialized contract instead of maintaining structurally identical argument lists.

## Shared helpers

Move decimal-normalized slippage validation to `sra-common/src/source_deposit.rs`. Preserve fail-closed overflow behavior and restore exact-division and zero-slippage regression cases in addition to the existing decimal-direction tests.

Move optional empty-or-owned account decoding to a focused handler utility. Preserve one safety comment explaining why normal `Account::try_from` cannot represent an intentionally empty optional PDA.

Add `SraConfig::last_triggered_nonce() -> Option<u64>`. Each adapter maps `None` to its existing adapter-specific `TriggeredNonceUnavailable` error. Adapter event schemas stay unchanged.

Remove the unused `SraConfig` argument from `build_deposit_now_bundle` and its callers.

## CCTP mint invariant

Retain `input_mint.address == USDC_MINT` and remove the additional `decimals == 6` constraint. A valid legacy SPL mint at the canonical address has immutable initialized decimals; the removed branch is reachable only by forging account state in the test VM. Tests retain canonical-address rejection and remove forged-decimals behavior.

## Test organization

Replace `passive_source.rs` with:

- `manage_sra.rs` for owner-managed initiator and pause state.
- `passive_trigger.rs` for generic authorization, route, account, fee, slippage, pairing, nonce, and rollback behavior.
- `passive_cctp.rs` for CCTP success, finality, quote, mint, and event-account behavior.
- `passive_across.rs` for Across success, timing, delegate, and CPI rollback behavior.

Move passive fixture extensions, pair builders, snapshots, rejected-state assertions, and raw state setters to `tests/common/passive.rs`. `PassiveFixture::for_mint` creates alternate-mint views without repeated full struct literals. A named canonical-CCTP-mint helper replaces the adapter-ID conditional inside the shared constructor. Remove the smaller duplicate `SourceFixture`.

Split the combined fee/slippage/pairing rejection test so each failure group runs independently. Restore the no-ALT source packet-size test and its expected oversized result.

## Delivery

Use separate commits for:

1. Shared slippage math and restored boundary tests.
2. Trigger ABI/account cleanup and regenerated IDLs.
3. Shared nonce/account helpers and adapter cleanup.
4. Passive test decomposition and restored packet-size coverage.

Run focused tests after each change, then rebuild the handler and both adapters, regenerate all three IDLs, run `cargo test --workspace`, formatting, strict Clippy, and `git diff --check`. Update the PR body to state that owner recovery from the passive SRA vault lands in Task 3.

## Non-goals

- Do not remove nonce fields from adapter events.
- Do not change destination instructions or state.
- Do not implement Task 3 recovery.
- Do not add deployment or live-fund operations.
