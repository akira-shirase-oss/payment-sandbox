# Architecture

## Sources of truth

1. **PostgreSQL is the only authority** — journal entries, holds, operations,
   idempotency keys, inbox/outbox and reconciliation all live there.
2. **The journal is immutable** — posted entries are never updated or
   deleted; corrections are reversing entries. Materialized balances are
   derived, and are reconciled to the journal rather than trusted.
3. **The simulator owns its own store** — its recorded result for an
   operation identity is the authority for that result; the server
   reconciles by readback, never by reissue.
4. **Virtual time belongs to the simulator** — test clocks advance billing
   scenarios; they cannot rewrite already-posted history.

## Components

| Piece | Role |
|---|---|
| `sandbox-domain` | Pure: `Money` (checked integers + explicit currency scale), account kinds, posting recipes, operation state machines, capacity rules, named refusals. No I/O — enforced. |
| `sandbox-db` | SQLx: migrations, journal append, hold/capacity transactions, idempotency + inbox/outbox, reconciliation queries. |
| `sandbox-server` | Axum API: payments, holds, captures, refunds, dashboard serving, project isolation, sandbox keys; the bounded outbox deliverer (lease, deadline, jitter retries). |
| `sandbox-sim` | **Separate process with its own state store**: approvals, declines and uncertain outcomes; seeded failure scenarios; virtual billing time; readback endpoint keyed by operation identity. |
| `sandbox-cli` | Rust CLI: create payment, capture, refund, inspect operations and the ledger, confirmed dataset reset. |
| `packages/sdk-ts` | TypeScript SDK for test stores. |
| `apps/checkout` | SvelteKit test checkout — the payer-facing sandbox page a test store redirects to. |
| `apps/dashboard` | SvelteKit inspection UI — operations, postings, balances, events, reconciler state. |

## Paths

**Authorize:** `POST /payments {operation_key, token, amount, currency}` →
commit the operation record (identity persisted **first**) → call the
simulator → apply the result in a second transaction: hold row + balanced
journal entries + outbox event. A lost reply leaves the operation
`uncertain`; the reconciler resolves it by readback under the same identity.

**Capture / refund:** `POST /captures|/refunds {operation_key, amount}` →
one transaction inserts the operation **and** reserves capacity against the
hold's sums (authorized − captured for captures; captured − refunded for
refunds) → the simulator call happens outside any transaction → a third
transaction applies the result: balanced postings + outbox event.

**Webhook:** each state change commits its outbox row in the same
transaction → the deliverer claims it under a lease → signed POST →
bounded retries with jitter → the consumer's inbox deduplicates by event id
→ fulfillment commits under its own business key → duplicate or reordered
deliveries reconcile instead of corrupting.

**Recovery / reconcile:** startup and periodic sweeps find `uncertain`
operations → read the simulator's stored result by operation identity →
apply it once → report, never blind replay. Materialized balances are
recomputed from the journal and compared; a mismatch is a defect, not a
rounding error.

**Reset:** dataset reset requires explicit confirmation; projects stay
isolated; simulator scenarios are seeded and preserved across runs.

## Consistency story

Postgres transactions make each logical step atomic: journal postings,
capacity reservations and the outbox commit together — the simulator call
sits *between* transactions, never inside one. The simulator is
authoritative for its own outcomes and idempotent under our operation
identity, so a lost response is a reconcilable `uncertain`, not a lost
charge. Cross-process truth is closed by readback; cross-system truth by
inbox dedup and the fulfillment business key. The ledger closes the loop:
every materialized balance must equal the sum of its journal entries.

## Failure cases

See `docs/END_GOAL.md` — the four cases (key conflict, concurrent
capture/refund, duplicate/reordered webhook, crash/clock-jump/restore) are
contract, not options.

## Layout

```
crates/        sandbox-domain, sandbox-db, sandbox-server, sandbox-sim,
               sandbox-cli
apps/          checkout, dashboard   (SvelteKit, static, same-origin served)
packages/      sdk-ts, the CMS sandbox provider adapter
docs/          END_GOAL, ARCHITECTURE, ROADMAP, DECISIONS
fixtures/      reference fulfillment consumer, seeded simulator scenarios
perf/          load scenarios + results (machine + commit on every run)
```
