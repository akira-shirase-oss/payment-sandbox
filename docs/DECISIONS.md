# Decisions

Deliberate choices seeded by the guide
(workspace `docs/Project_Guides/05_Payment_Sandbox.docx`), newest last. D-NNN numbering
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

## Decisions made before code (2026-11-16)

These close the questions that blocked milestone 0 and fix the gaps a plan
review found. Each states the choice, the alternative and the cost.

- **D-010 — Currency set and scale.** The sandbox supports exactly USD (2
  decimal places), EUR (2), JPY (0) and KWD (3). Amounts are `i64` minor
  units with the exponent taken from a fixed table in `sandbox-domain`;
  there is no FX, and a posting that mixes currencies is a named refusal.
  A single operation amount is at most 10^12 minor units, so no sum of a
  dataset's postings can overflow `i64` before the per-entry check does.
  JPY and KWD widen the test surface; they are what proves scale is handled
  rather than assumed.
- **D-011 — Holds are two-phase journal postings.** Following the
  two-phase transfer model the guide points to (TigerBeetle): authorize
  posts *payer funding → payer held*; capture posts *payer held → merchant
  pending*; void posts the uncaptured remainder *payer held → payer
  funding* (a reversing entry); refund posts *merchant pending → payer
  funding*. A payment is the parent record; every authorize, capture, void
  and refund is its own operation record with its own identity; the
  payment row carries the committed sums (authorized, captured, voided,
  refunded) and the in-flight reservations. Fulfillment belongs to the
  consumer, never to this ledger. Costs more postings than holds kept
  outside the journal; buys pending balances that the journal can explain,
  which the ship gate needs.
- **D-012 — Chart of accounts.** Per project and currency: `payer_funding`
  (the synthetic source; may run negative), `payer_held`,
  `merchant_pending`, `merchant_available`, `fees`, `refunds_clearing` and
  `network_settlement`. The chart is data in `sandbox-domain`, every
  posting recipe is checked against it by property tests, and accounts not
  yet used by a recipe (available, fees, clearing, settlement) are reserved
  for the billing milestone.
- **D-013 — Project isolation by composite keys.** Every row carries
  `project_id`; primary keys are `(project_id, id)` and every foreign key
  includes `project_id`, so a cross-project reference is impossible in the
  schema, not merely unlikely in code. Repositories take a typed
  `ProjectScope`. Row-level security may be added later as defence in
  depth; composite keys cost nothing now.
- **D-014 — Capacity is reserved under a row lock with checked sums.** A
  capture, void or refund locks the payment row (`SELECT … FOR UPDATE`),
  checks the remaining capacity, records the operation and increments the
  matching reservation in one transaction. `CHECK` constraints hold
  `captured + capture_reserved + voided + void_reserved ≤ authorized` and
  `refunded + refund_reserved ≤ captured`, so even a code bug cannot
  overrun. Payments serialize per row; the hot-payment and hot-account
  races (D-027) measure that cost. SERIALIZABLE with retries was the
  alternative; it aborts more under the 5× burst.
- **D-015 — Operation identity is derived, never generated.** The
  operation id is UUIDv5 over (project id, operation kind, the client's
  `Idempotency-Key`), and the payment id is UUIDv5 over its authorize
  operation id. A retry after the server database was restored to an
  older backup therefore produces the *same* identity, and the simulator
  answers it from its store instead of charging again. A reverse
  reconciliation lists simulator operations the server does not know.
  Server-generated UUIDs would double-charge after a restore.
- **D-016 — Idempotency scope and binding.** Money-moving requests require
  an `Idempotency-Key` header (1–255 visible ASCII characters). Its scope
  is project × operation kind. The key binds to a SHA-256 hash of the
  operation's canonical fields (kind, payment, amount, currency, token);
  the same key with a different hash is `409 idempotency_key_conflict`,
  the same key with the same hash replays the stored operation. Keys for
  money-moving operations never expire within a dataset epoch (D-022).
- **D-017 — Fenced readback.** "Not found" on readback is not proof of
  non-execution: a delayed request could still arrive. The simulator's
  readback writes a tombstone for an unknown operation id and refuses any
  later request under it. Only a decline or a fenced "never executed"
  releases a capacity reservation; an `uncertain` operation keeps its
  reservation until one of those, so reservations cannot leak.
- **D-018 — The simulator's store is its own SQLite file.** `sandbox-sim`
  owns a SQLite database (WAL, `synchronous=FULL`) that nothing else opens.
  It is a separate failure domain from the server's PostgreSQL, trivial to
  seed and wipe, and makes the process boundary of D-002 honest. A second
  PostgreSQL database would share the server's failure mode.
- **D-019 — No card-number field exists anywhere.** Checkout offers a
  scenario picker (approve, decline, uncertain …) that maps to synthetic
  tokens `tok_sim_*`; the API accepts only those tokens. Any request field
  named like card data (`number`, `card`, `cvc`, `cvv`, `exp_*`, `pan`) and
  any run of 13–19 digits that passes the Luhn check — contiguous, or in a
  printed card grouping (4-4-4-4, 4-6-5 …) — in any string or key of a
  request body or in the `Idempotency-Key`, is rejected with
  `422 raw_card_data_rejected` before typed parsing; the rejected value is never logged or persisted, and
  a test proves both. Rejecting at the API alone would be too late for a
  free-text field, because the number would already have reached us.
- **D-020 — Webhook signatures.** HMAC-SHA256 over
  `"{event_id}.{unix_timestamp}.{body}"`, sent as
  `Sandbox-Signature: t=<unix>,v1=<hex>` with the event id in
  `Sandbox-Event-Id`. Verifiers reject timestamps more than 300 s from
  their clock (replay window). During a secret rotation both secrets sign
  and the header carries two `v1=` values, so consumers can roll without
  downtime. Signing secrets (`whsec_sim_…`) are encrypted at rest with
  ChaCha20-Poly1305 under a server key — they cannot be hashed because the
  server must compute HMACs with them. This corrects the earlier blanket
  rule "secrets are hashed": API keys are hashed, signing secrets are
  encrypted. Header names deliberately differ from any real processor's.
- **D-021 — Sandbox keys, scopes and rotation.** Keys are `sk_sim_`
  followed by 32 random bytes in lowercase hex; only their SHA-256 hash is
  stored (fine for high-entropy keys) and the plaintext is shown once.
  Scopes: `read`, `payments:write`, `admin` (keys, webhooks, dataset
  reset). Rotation means creating a second key and revoking the first;
  several keys may be active. Projects are created by an operator with a
  bootstrap token configured on the server (local/CI deployment, D-022).
  Dashboard sign-in is decided with the dashboard milestone.
- **D-022 — Dataset reset is a new epoch, not a delete.** A reset bumps the
  project's `dataset_epoch`; earlier journal rows stay immutable and
  archived, current reads and idempotency scopes use the new epoch. It
  requires the `admin` scope, a typed confirmation of the project slug in
  CLI and UI, and writes an audit row. Immutability holds within and
  across epochs.
- **D-023 — Deployment mode: local and CI only.** The first releases are
  self-hosted for development machines and CI. In this mode outbound
  webhook delivery may reach loopback addresses only; every other target,
  including private ranges, link-local and anything a redirect points to,
  is refused at connect time after DNS resolution. A hosted mode comes
  later, with a private-range guard and no loopback exception.
- **D-024 — The development store is `fixtures/dev-store`.** The reference
  fulfillment consumer lives in this repository and *is* the "development
  store" the ship gate needs: it receives signed webhooks, deduplicates in
  its own inbox, re-reads payment state from the API, and fulfills under
  its own unique business key. Colophon's commerce is parked, so a CMS
  adapter is optional later and integrates only through public APIs.
- **D-025 — Thin events with a per-payment sequence.** Each webhook event
  carries `{id, type, payment_id, sequence, simulated: true}`; consumers
  re-read the payment from the API on receipt, so duplicated or reordered
  deliveries converge on current state. Every payload and response carries
  `"simulated": true`, and the SDK refuses production-looking
  configuration unless explicitly overridden.
- **D-026 — Failure drills are assigned, and three of them are distinct.**
  "Crash, clock jump or backup restore" is three drills: (a) *process
  crash* — the server killed after the simulator accepted an operation but
  before the result was stored (M3); (b) *clock jump* — the simulator's
  virtual clock jumped forward and back must not change posted journal
  history (M3), and a server wall-clock jump against outbox leases may
  cause a duplicate delivery but never a lost or corrupt one (M4);
  (c) *backup restore* — the server's PostgreSQL restored from a backup
  taken before an operation, with a newer simulator store; a client retry
  must reconcile without a second charge (M3). The automated drill takes
  the backup as a `CREATE DATABASE … TEMPLATE` copy (a consistent snapshot
  of every committed row, taken with no connection open); a `pg_dump`
  logical-restore rehearsal against the local stack remains to be scripted.
- **D-027 — Two hot races.** "Hot-account refund race" means one
  merchant account receiving many concurrent partial refunds across many
  payments; "hot-payment race" means many concurrent partial refunds
  (or captures) against one payment. Both are tested and measured; the
  named example is 64 concurrent refunds of 10 against 300 captured:
  exactly 30 succeed and 34 fail with `insufficient_capacity`.
- **D-028 — Account balances are summed from the journal.** No account
  balance row is updated per posting: a single hot row would serialize
  every capture in a project behind one lock held through commit. Account
  balances are computed from `journal_lines` (indexed by project, account)
  and checkpointed by reconciliation; per-payment sums live on the payment
  row and are reconciled to the journal. A mismatch is a defect.
- **D-029 — Checkout and dashboard get separate origins.** Checkout renders
  merchant-supplied strings to payers; the dashboard holds the merchant's
  session. They are served by the Rust server on separate listeners
  (separate origins) with a strict CSP on checkout, so an XSS in checkout
  cannot reach a dashboard session. This replaces the "same origin" line.
- **D-030 — Versioned ledger semantics.** `sandbox-domain` exports
  `LEDGER_SEMANTICS_VERSION`; it changes whenever a posting recipe or the
  chart changes meaning, every journal entry records the version it was
  posted under, and the API is versioned under `/v1`. Migrations get
  restore tests from milestone 3 on.
- **D-031 — Milestones are finer, and "Done" cites evidence.** The roadmap
  splits the work into M0–M8 so that each milestone carries its own
  failure drills and load scenario. A status cell says Done only when it
  names the tests and the evidence path. Asynchronous simulator outcomes
  (settlement, disputes, authorization expiry on virtual time) need a
  server-side inbox; that is scheduled with the billing milestone, not
  forgotten.
- **D-032 — Webhook egress, retries and bounds.** One delivery attempt has
  a 5 s deadline and a 64 KiB response-read cap; retries use exponential
  backoff with full jitter from 1 s to 10 min, at most 12 attempts, after
  which the delivery is `dead` and visible. The deliverer claims work
  under a lease with a fencing token, so a stale worker cannot overwrite a
  newer attempt's result. Redirects are never followed.

- **D-033 — One payment's requests queue in the server, not in the
  database; recording an answer is retried.** The first refund-race smoke
  (M2) found two defects. (1) Requests for one hot payment waited for its
  row lock *inside PostgreSQL*, each holding a pooled connection, and
  exhausted the pool for every other payment (1,030 pool timeouts). Now
  captures, voids, refunds and answer-recording for one payment first take
  a turn on one of 1,024 fair in-process async mutexes chosen by payment
  id, then a connection; the wait is bounded (`PS_PAYMENT_QUEUE_WAIT_MS`,
  1 s) and an expired wait is `503 payment_busy`, retryable under the same
  key. (2) When recording the network's answer hit a transient failure
  (lock or statement deadline, busy pool), the operation stayed open with
  its reservation held, so 41 limbo reservations refused legitimate
  refunds. Now recording is retried up to six times with exponential
  backoff and full jitter (about 600 ms in total); it is idempotent, so a
  retry can never double-post, and giving up leaves the operation open for
  a client retry or the reconciler. After both fixes the same smoke
  admitted exactly 630 of 2,001 refunds with zero lock waits over 50 ms.
  Regression tests: `a_hot_payment_cannot_starve_the_pool_for_other_payments`
  and `recording_an_approval_survives_a_transient_lock_timeout`, each shown
  to fail without its fix. Cost: per-payment throughput is bounded by one
  transaction at a time per server process, which is what the row lock
  allowed anyway; several server processes would still contend in the
  database, with far fewer waiters each.

- **D-034 — The simulator's store is a redb file, not SQLite.** D-018's
  intent stands (a separate failure domain the simulator alone opens); its
  technology changes. With sqlx 0.9 in the server, the only SQLite
  reachable is the bundled 3.53 of `libsqlite3-sys` 0.37, which carries
  RUSTSEC-2026-0459, and Cargo refuses a second `links = "sqlite3"` crate
  (rusqlite with the patched 3.54) in the same lockfile even though sqlx's
  SQLite driver is not enabled. redb is pure Rust, ACID and crash-safe,
  removes the C library, and fits the single-writer design: one thread owns
  the file, a bounded queue (4,096) feeds it, up to 128 commands share one
  durable commit (group commit), and replies go out only after the commit.
  Measured on the development Mac at load average about 30 (busy, a
  development observation): the simulator alone sustained about 3,000
  operations/s with p99 57 ms, against about 1,000/s for the SQLite version;
  each commit costs one `F_FULLFSYNC` on macOS (Rust's `sync_data` flushes
  the drive cache there, which SQLite's default does not), so the median
  rose from under 1 ms to 12–21 ms. Full durability is kept: the drills
  kill processes, and the network's record must survive them.

- **D-035 — One web stack: SvelteKit + Tailwind v4 + shadcn-svelte, from a
  copied UI package.** Maintainer decision, 2026-11-16 (workspace rule "one
  stack for every project"): Rust stays the backend; the test checkout and
  the inspection dashboard are SvelteKit + TypeScript (strict) + Tailwind
  CSS v4 + shadcn-svelte. They start from a copy of the Colophon UI package
  (the warm paper-and-ink look, D-056 there), copied into this repository
  as `packages/ui`, which this repository then owns; there is no dependency
  on the Colophon repository, its packages or its builds. No UI existed
  when this was decided, so nothing needs converting; the checkout still
  has no card field (D-019) and the two apps keep separate origins (D-029).

- **D-036 — Reconciler cadence and bounds.** The server runs one tracked
  reconciler task: a sweep at startup, then every `PS_RECONCILE_INTERVAL_MS`
  (5 s). A sweep examines at most `PS_RECONCILE_BATCH` (200) open operations,
  oldest first, and only those older than `PS_RECONCILE_GRACE_MS` (the
  network deadline plus 1 s), so a request still waiting for its answer is
  never fenced from under it. Each operation gets a fenced readback; a
  network error leaves it for the next sweep; recording uses the same
  jittered retry as requests (D-033). Reverse reconciliation
  (`GET /v1/reconciliation?network=true`) pages through the network's
  listing (pages of up to 1,000 operations, at most 1 MiB each, at most 100
  pages) and reports at most 1,000 unknown identities. It never applies
  them: only a client retry under the same key can, and that retry
  re-derives the same identity.

- **D-037 — Reports have their own deadline and a statistics-proof plan.**
  The first M4 load smoke found `GET /v1/reconciliation` cut off at the hot
  path's 5 s request deadline: on a project written moments earlier, stale
  planner statistics chose a nested loop over sequential scans for the
  per-payment check, which took 16 ms once statistics caught up. The check
  is now index-driven per payment (`payments` → `journal_entries_by_payment`
  → the lines' primary key) whatever the statistics say, and report routes
  get their own bounded 60 s request deadline (statements inside stay at
  30 s), so a whole-epoch scan never borrows the hot path's budget. No
  deterministic regression test exists for a planner's choice; the evidence
  is the rerun smoke, whose report answered within its deadline.

## Open questions (still open)

- **Dashboard authentication** — per-project-owner session login is the
  default assumption; decided with the dashboard milestone.
- **Billing scenario order and definitions** — the default order is
  settlement → fees → payouts → disputes → invoices → recurring, because
  each later scenario reads settled balances; each gets its own contract
  when reached.
- **Hosted mode** — quotas per project, the private-range egress guard and
  sign-up abuse limits, if a hosted instance is ever offered.
