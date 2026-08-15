# AGENTS.md

Implementation guide for `extrema_guard`.

## Scope

`extrema_guard` is a composable runtime for scheduled account and order guard
policies built on `extrema_infra`. The framework itself does not impose a global
reduce-only or guard-owned-order policy. Each module defines the account objects
it may manage and must make that policy explicit in configuration, tests, and
audit output.

The built-in static-protection and profit-lock modules are reduce-only and use
guard-prefixed client order ids because those choices fit their behavior. Do not
generalize those module-specific choices into framework-wide restrictions.

## Runtime behavior

- Load behavior from `guard.toml`; malformed configuration is a startup error.
- Dry-run is the default. Switching a deployed instance to live mode requires an
  explicit operator request and `i_understand_live_orders = true`.
- Use `GuardExecutor` for normalized shared exchange operations. Add narrowly
  scoped executor methods when a module needs exchange-native fields.
- Apply per-exchange throttling to outgoing actions and write an audit record for
  both requests and confirmed outcomes.
- Policies based on elapsed observation time must persist their state atomically
  and validate it on load. They must not silently restart a timer after process
  failure if doing so changes the policy outcome.

## Adding a module

1. Follow the built-in layout: `base.rs` for state and reconciliation, `core.rs`
   for strategy traits, and `utils.rs` for pure decision logic.
2. Filter scheduler events by `msg.task_id`.
3. Define eligibility conservatively and log a reason for every skipped object.
4. Re-read mutable exchange state immediately before a cancel or placement.
5. Treat partial fills and action races as explicit terminal outcomes requiring
   reconciliation, not generic errors.
6. Put deterministic decision cases in unit tests and exchange wiring behind a
   dry-run or explicit live gate.

For a private module, a separate crate depending on `extrema_guard` is also a
supported layout.

## Build and verify

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy -- -D warnings
```
