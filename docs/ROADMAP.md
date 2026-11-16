# Roadmap

Milestone order follows the guide's four build steps: accounting core
(step 1 → M0), payment lifecycle (step 2 → M1–M2), separate simulator
(step 3 → M3), developer product (step 4 → M4–M7); the guide's "first
useful release" is M6, and M8 is the full-direction billing work. The
finer split (D-031) gives every milestone its own failure drills and load
scenario instead of leaving them for the end.

**Status rule.** A status cell may say *Done* only when it names the tests
and the evidence path that prove the gate. Anything else is *Planned* or
*In progress*. Load evidence measured while the machine was busy is a
smoke observation, never capacity, and the full run stays listed as owed.

Every milestone ships: property tests that every journal stays balanced and
every balance equals its journal sum; the guide's failure cases that apply
to it as automated tests; a load scenario with thresholds under `perf/`.

| Milestone | Builds | Proves (gate) | Status |
|---|---|---|---|
| **M0 — the spine** | Cargo workspace; pure `sandbox-domain` (`Money` over D-010's currency table, chart of accounts, journal entries, posting recipes, operation state machine); PostgreSQL schema with an append-only, balance-checked journal; Axum skeleton (bounded bodies, deadlines, idempotency-key parsing, one error envelope); `scripts/check.sh`; CI; `deploy/compose.yaml` | Every posted entry balances per currency (property tests + a commit-time trigger); unbalanced, mixed-currency and overflowing postings are named refusals; `UPDATE`/`DELETE`/`TRUNCATE` on the journal fail in the database; the domain crate has no I/O dependencies; first load scenario and first failure drill (database outage) | See below |
| **M1 — projects and sandbox keys** | Projects created with a bootstrap token; `sk_sim_` keys hashed, with scopes and revocation; project-scoped repositories over composite keys | A key of project A can never read or write project B (schema and API tests); a revoked key fails at once; secrets never appear in logs (log-capture test); load scenario on authenticated reads | Planned |
| **M2 — payment lifecycle** | Payments, authorize / capture / void / refund as two-phase holds (D-011), operations with derived identity (D-015) and payload-bound keys (D-016), transactional capacity (D-014); `sandbox-sim` as a separate process with its own SQLite store answering approve/decline by scenario token; raw-card rejection (D-019) | Same key + different payload → conflict; the D-027 races (hot payment, hot account) cannot overrun capacity and the excess fails with a named refusal; capture-vs-void and refund-vs-pending-capture races; zero journal imbalance and every payment's sums equal its journal sums; PAN-like input rejected and never persisted or logged; load scenario: lifecycle at a fixed rate plus both races | Planned |
| **M3 — uncertain outcomes and recovery** | Simulator lost replies, timeouts and seeded failure scenarios; fenced readback (D-017); reconciler at startup and periodically; reverse reconciliation; simulator virtual clock | Lost replies stay `uncertain` until readback, then apply exactly once; **drill (a)**: server killed after the simulator accepted, before the result was stored → recovers with no second charge; restart of either or both processes mid-operation; **drill (b, simulator half)**: a virtual-clock jump forward and back leaves posted journal history byte-identical; **drill (c)**: server database restored from an older dump, newer simulator store → a client retry reconciles with no second charge, and reverse reconciliation reports what the server lost | Planned |
| **M4 — signed webhooks** | Outbox committed with every state change; leased deliverer with fencing, deadline, bounded jittered retries (D-032); signatures and rotation (D-020); egress guard (D-023); `fixtures/dev-store` (D-024) with inbox dedup, thin-event re-read (D-025) and a unique fulfillment key | Delivery fuzzer: random duplication, reordering and delay over N payments ends with fulfillments equal to captured payments and zero duplicates; receiver down → retries then delivers, nothing lost; signature, replay-window and rotation tests; loopback-only egress refuses private, link-local, public and redirect targets; **drill (b, wall-clock half)**: a lease-clock jump causes at most a duplicate delivery, never a lost or corrupt one | Planned |
| **M5 — reconciliation and explain-balance** | Reconciliation report (journal balance, payment sums vs journal, account checkpoints, open uncertain operations, simulator agreement both ways); `explain-balance` for any account | After every load run, every account's balance equals the sum of its journal entries with zero mismatches; any balance can be explained line by line; a planted drift is detected | Planned |
| **M6 — first useful release** | SvelteKit checkout (scenario picker, **no card field**, D-019) and inspection dashboard (operations, postings, balances, events, reconciler state) on separate origins (D-029) | The guide's first useful release end to end: one synthetic purchase, partial refund, balanced ledger visible in the UI, durable webhook fulfilled by the dev store, and the M3 kill drill repeated through the checkout path; browser tests prove no card field exists | Planned |
| **M7 — developer product** | Rust CLI, TypeScript SDK, seeded scenario library, dataset reset as a new epoch with typed confirmation (D-022), sandbox labels end to end, migration/restore tests per release | A development store tests payment failures reproducibly: each library scenario run twice yields the same sequence of states and events (ids aside); reset needs `admin` + typed slug and never deletes journal rows; production-looking SDK config refused | Planned |
| **M8 — billing direction** | Server-side inbox for asynchronous simulator outcomes; authorization expiry on virtual time; then settlement → fees → payouts → disputes → invoices → recurring, each with its own contract | Each new scenario reconciles to the journal; virtual time never rewrites already-posted history | Planned |

### M0 status

*Planned.* Updated as each slice lands, with the commit that proves it.

## Load plan and ship gate

`perf/` scenarios per the guide: two projects, 100,000 seeded ledger
postings, the mock network — 100 operations/s sustained, a 5× short burst,
the hot-account and hot-payment refund races (D-027). Record commit
p95/p99, lock waits, journal imbalance and duplicate logical operations;
the correctness gate is zero imbalance, zero unauthorized transfers, zero
duplicate fulfillments. Full runs are ≥1,000,000 operations under the
workspace standard, with results committed to `perf/results/` (machine +
commit + machine load at the time). Guide numbers are proposed lab
workloads, not measured performance. Benchmark traffic never leaves the
machine: the network is `sandbox-sim` on loopback.

**Ship gate:** a development store (`fixtures/dev-store`) can test payment
failures reproducibly — the same scenario seed and inputs produce the same
sequence of operation states and events — and every synthetic balance can
be explained from journal entries — `explain-balance` returns the postings
that sum to it, and the post-load reconciliation finds zero mismatches.
