# extrema_guard

Composable account and order guard runtime built on
[extrema_infra](https://crates.io/crates/extrema_infra). It provides a scheduler,
exchange clients, configuration, action throttling, and audit logging so guard
policies can focus on eligibility and reconciliation logic.

## Built-in modules

- **Static protection** derives stop-loss and take-profit levels from position
  entry price. Its take-profit and close actions are reduce-only because that is
  the policy of this module.
- **Profit lock** tracks the best observed mark during a position lifecycle and
  closes the position after an activated profit floor is crossed.

Additional modules may inspect positions, all open orders, order history, and
market data, then place or cancel orders according to their own explicit policy.
Order ownership is a module-level decision: the built-in protection modules use
guard-prefixed client order ids, while a stale-entry policy may intentionally
manage eligible user-created orders by exchange order id.

## Runtime model

- `guard.toml` selects exchanges, schedule frequency, live or dry-run mode, and
  module rules.
- `GuardExecutor` owns exchange clients and provides normalized positions and
  order operations.
- Modules run from extrema_infra scheduler events and reconcile observed account
  state with their configured policy.
- State may be reconstructed from exchange data or persisted by a module when
  the policy depends on continuous observations across process restarts.

## Operational controls

| Control | Behavior |
|---|---|
| Dry-run | Default mode records intended actions without sending them |
| Live arming | `mode = "live"` also requires `i_understand_live_orders = true` |
| Action rate | Per-exchange minimum interval between outgoing actions |
| Order size | Shared notional cap for modules that place orders |
| Market sanity | Invalid or non-finite prices are rejected |
| Audit | Actions append to the configured log path |

These are runtime controls, not product-level restrictions. A module remains
responsible for strict eligibility checks, preflight revalidation, idempotency,
and confirmation of exchange outcomes.

## Quickstart

```bash
cp guard.toml.example guard.toml
cargo run --bin guard
```

The example configuration uses `mode = "dry-run"`. Live mode sends real account
actions and should be enabled only after reviewing the selected modules and their
logs.

## Extending

Two layouts are supported:

1. Copy `src/bin/guard_custom_template.rs`, implement a module, and register it
   with `EnvBuilder::with_strategy_module`.
2. Depend on `extrema_guard` as a library and compose public or private modules
   in another crate.

Keep pure decision logic separate from exchange IO. Policies that can cancel or
place orders should default to dry-run, re-read mutable order state immediately
before acting, and record both the requested action and confirmed outcome.

## Verification

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy -- -D warnings
```
