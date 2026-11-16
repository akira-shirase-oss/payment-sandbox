# Decisions

Deliberate choices seeded by the guide
(`Project_Guides/05_Payment_Sandbox.docx`), newest last. D-NNN numbering
continues the workspace convention.

- **D-001 — Simulated money only, by construction.** No real credentials,
  no custody, no redeemable balances; raw-card fields are rejected at the
  API and no production provider route exists to misconfigure. The cost is
  fidelity — a real processor would be a different business and a different
  design, and the guide says so.
- **D-002 — The network simulator is a separate process with its own
  store.** Not an in-process mock: only a real process boundary produces
  the failure modes the contract demands — lost replies, mid-operation
  restarts, readback reconciliation. Costs an extra service and a second
  state store; buys honest `uncertain` states that can actually happen.
- **D-003 — The double-entry journal is the authority; balances are
  derived.** Postings are immutable and balanced per currency; corrections
  happen only as reversing entries; materialized balances serve reads and
  are reconciled back to the journal. Costs a reconciliation pass; buys a
  ledger that can always explain itself.
- **D-004 — Operation identity commits before the network call.** The
  operation record lands first; the simulator persists its own result under
  that identity; a lost response stays `uncertain` until readback — never
  retried blind. No database transaction ever spans a simulator call.
- **D-005 — Capacity is reserved inside the transaction.** Capture and
  refund amounts are checked against the hold's sums in the same
  transaction that records the operation, so concurrency can only fail the
  excess — never overrun it. Slower under contention by design; correct
  under contention by construction.
- **D-006 — At-least-once delivery, deduplicated consumption.** Outbox
  event ids deduplicate logical work but repeated external delivery is
  expected behavior, not a bug; the consumer's inbox dedups and fulfillment
  carries its own business key. Exactly-once delivery is never claimed.
- **D-007 — Idempotency conflicts; it does not reinterpret.** An operation
  key binds to the request payload; the same key with a different amount or
  body returns a conflict, so an accepted operation can never be silently
  rewritten by a retry.
- **D-008 — Virtual billing time cannot rewrite history.** Test clocks
  drive settlement and recurring scenarios in the simulator; posted journal
  history is commit-ordered and immutable under clock jump, crash or
  backup restore.
- **D-009 — Sandbox labeling is load-bearing.** Sandbox-only key prefixes,
  prominent warnings in binaries, UI, README and release artifacts,
  project isolation, hashed secrets, outbound allowlists and a confirmed
  dataset reset. The CMS integration is an explicit sandbox provider
  adapter — no production store may fulfill a real order from simulated
  evidence.

## Open questions (the guide leaves these open)

Recorded here so they are decided explicitly, not guessed at:

- **Simulator state store technology** — the guide requires a separate
  process *and* state store but not its kind (own Postgres database,
  SQLite, files). Decide at M2.
- **Entity taxonomy detail** — "payment, authorization hold, capture,
  refund and fulfillment are distinct records": the parent/child shape
  (does an authorize create both a payment and a hold record?) is a
  modelling decision for M1's schema.
- **Capacity mechanism** — aggregate checks on the hold row, a capacity
  column, or constraint-backed counters: the invariant is fixed, the
  mechanism is M1's choice.
- **Sandbox key format, scopes, rotation** and dashboard authentication —
  "sandbox-only keys" and "isolate projects" are requirements without a
  specified shape; project isolation mechanism (row-level security vs
  application scoping) is likewise open.
- **Webhook signature scheme** and key rotation — "signed webhooks" is
  the requirement; the scheme is ours (HMAC is the default assumption).
- **Where the reference fulfillment consumer lives** — `fixtures/` in
  this repo or the CMS repository; the contract needs its inbox dedup
  and business key proven somewhere at M3.
- **Billing scenario order and definitions** — fees, settlement, payouts,
  disputes, invoices, recurring plans are the full direction but
  "incremental"; each contracts when reached in M5.
- **Supported currencies and scales** — balancing is per currency, but
  the sandbox currency set and scale policy are unspecified.
- **Dataset reset confirmation mechanism** — "explicit confirmation" is
  required; the shape (typed token, CLI flag, UI step) is open.
