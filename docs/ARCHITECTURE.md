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
| `sandbox-http` | Bounded HTTP/1 serving for all three processes (D-054): connection cap before accept, read and write deadlines, per-connection request and age limits, bounded drain. |
| `sandbox-server` | Axum API: payments, holds, captures, refunds, dashboard serving, project isolation, sandbox keys; the bounded outbox deliverer (lease, deadline, jitter retries). |
| `sandbox-sim` | **Separate process with its own state store** (a redb file, D-034): approvals, declines and uncertain outcomes by `tok_sim_*` scenario token; its own capacity checks against its own record; fenced readback (D-017); listing for reverse reconciliation; virtual test clock; test-only fault rules (delay, lost reply, unavailable). One writer thread, bounded queue, group commit; listings and clock reads run in their own read transactions outside the writer (D-047). Executions, readbacks and listings need the server's network credential; faults and the clock are a control plane mounted only when switched on (by default only in the `dev` profile), behind an operator credential (D-046). |
| `sandbox-cli` *(planned, M7)* | Rust CLI: create payment, capture, refund, inspect operations and the ledger, confirmed dataset reset. |
| `packages/sdk-ts` *(planned, M7)* | TypeScript SDK for test stores. |
| `apps/checkout` *(planned, M6)* | SvelteKit test checkout — the payer-facing sandbox page a test store redirects to. |
| `apps/dashboard` *(planned, M6)* | SvelteKit inspection UI — operations, postings, balances, events, reconciler state. |

## Paths

**Authorize:** `POST /v1/payments {token, amount, currency}` with an
`Idempotency-Key` header → derive the operation id (UUIDv5 over project,
kind and key — D-015) and the payment id from it → commit the operation
and payment records (identity persisted **first**) → call the simulator
with that id → apply the result in a second transaction: hold sums +
balanced journal entry + outbox event. A lost reply leaves the operation
`uncertain`; the reconciler resolves it by fenced readback (D-017) under
the same identity. Because the identity is derived from the client's key,
a retry after a restore of the server database reaches the simulator's
stored result instead of a second charge.

**Capture / void / refund:** `POST /v1/payments/{id}/captures|void|refunds
{amount}` with an `Idempotency-Key` → one transaction locks the payment
row, inserts the operation **and** reserves capacity against the payment's
sums (authorized − captured − voided − reservations for captures and
voids; captured − refunded − reservations for refunds; D-014) → the
simulator call happens outside any transaction → a further transaction
applies the result: balanced postings + released reservation + outbox
event. A decline or a fenced "never executed" releases the reservation;
`uncertain` keeps it until readback.

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

**Reset:** dataset reset requires the `admin` scope and a typed
confirmation of the project slug, and opens a new dataset epoch — posted
entries are archived, never deleted (D-022); projects stay isolated;
simulator scenarios are seeded and preserved across runs.

**Recovery after a restore:** the reverse reconciliation lists simulator
operations whose identity the server does not know (its database was
restored to an earlier point); they are reported, and a client retry
under the same key re-derives the same identity and applies the
simulator's stored result once.

**Hot payments:** requests for one payment take a turn on an in-process
queue (1,024 fair async mutexes keyed by payment id) *before* they take a
database connection, so waiters for a hot row never hold pooled
connections; the wait is bounded and an expired wait is
`503 payment_busy` (D-033). Recording the network's answer is retried on
transient database failures with exponential backoff and full jitter.

## Ports (all loopback, D-023)

| Port | Process |
|---|---|
| 56432 | PostgreSQL (`deploy/compose.yaml`, compose project `payment-sandbox`) |
| 56080 | `sandbox-server` API |
| 56081 | `sandbox-sim` |
| 56090 / 56091 | server / simulator during load runs (`scripts/load.sh`) |

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
crates/        sandbox-domain, sandbox-db, sandbox-server, sandbox-sim
               (sandbox-cli: planned, M7)
apps/          checkout, dashboard (planned, M6: SvelteKit + Tailwind v4 +
               shadcn-svelte, separate origins — D-029, D-035)
packages/      ui (planned, M6: this repository's copy of the Colophon UI
               package); sdk-ts and the CMS sandbox adapter (planned, M7)
docs/          END_GOAL, ARCHITECTURE, ROADMAP, DECISIONS
fixtures/      dev-store (the reference fulfillment consumer and the
               ship gate's development store, D-024), seeded scenarios
perf/          load scenarios + results (machine + commit on every run)
```
