# Roadmap

Milestone order follows the contract: accounting core, payment lifecycle,
separate simulator, developer product — each with its proof before the next
starts. The guide's four build steps map onto M1–M4; M0 is the scaffold they
assume and M5 is the full-direction billing work.

| Milestone | Builds | Proves (gate) | Status |
|---|---|---|---|
| **M0 — scaffold + accounting core** | Workspace, docs, `Money` type (checked integers + explicit currency scale), accounts, journal schema and migrations, posting recipes, property tests | Every posted journal balances per currency; invalid postings and unbalanced recipes are named refusals; corrections exist only as reversing entries | Planned |
| **M1 — payment lifecycle** | Payments, authorization holds, captures, refunds as distinct records; operation keys; transactional capacity reservation | Same key + different payload → conflict; concurrent capture/refund cannot overrun capacity — the excess fails; zero journal imbalance under concurrency tests | Planned |
| **M2 — separate simulator** | `sandbox-sim` process + own store; seeded declines, failures and uncertain outcomes; result readback by operation identity; server-side reconciler | Lost replies stay `uncertain` until readback, then apply exactly once; restarting either or both processes mid-operation reconciles with no duplicate logical charge | Planned |
| **M3 — first useful release** | Test checkout, inspection dashboard, signed webhook outbox + deliverer, reference consumer with inbox dedup and its own fulfillment business key | The contract drill: kill the server after the simulator accepts but before the response is stored → recover without a second logical charge or fulfillment; duplicate/reordered webhooks deduplicated and reconciled | Planned |
| **M4 — developer product** | Rust CLI, TypeScript SDK, explicit CMS sandbox provider adapter, seeded scenario library, sandbox keys and labels end to end | A development store tests payment failures reproducibly through the adapter; raw-card rejection, project isolation, secret handling and confirmed dataset reset verified | Planned |
| **M5 — billing direction** | Simulated fees, settlement, payouts, disputes, invoices, fixed recurring plans on virtual test clocks — each gets its own contract when reached | Each new scenario reconciles to the journal; virtual time never rewrites already-posted history | Planned |

## Load plan and ship gate

`perf/` scenario per the guide: two projects, 100,000 seeded ledger
postings, the mock network — 100 operations/s sustained, a 5× short burst,
and a hot-account refund race. Record commit p95/p99, lock waits, journal
imbalance and duplicate logical operations; the correctness gate is zero
imbalance, zero unauthorized transfers, zero duplicate fulfillments. Full
runs are ≥1,000,000 operations under the workspace standard, with results
committed to `perf/results/` (machine + commit). Guide numbers are proposed
lab workloads, not measured performance.

**Ship gate:** a development store can test payment failures reproducibly
and every synthetic balance can be explained from journal entries.
