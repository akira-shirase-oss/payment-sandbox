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
  A single operation amount is at most 10^12 minor units, so no single
  entry can overflow `i64`. Account sums can: about 9.2 million postings
  of the maximum amount would exceed it. That fails closed: the sum is
  cast to `bigint` and the query errors, with no wrapping (review 17
  corrected the earlier claim that no dataset could reach it).
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
  of every committed row, taken with no connection open); the `pg_dump`
  logical-restore rehearsal is `scripts/restore-rehearsal.sh` (D-053).
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

- **D-038 — The server is not the owner.** Review finding: the server
  connected as the stack's superuser, which owns every table, so replica
  mode, `DISABLE TRIGGER`, `TRUNCATE`, backdating and cross-project lines
  all got past the journal's protections. Now three roles: the stack's
  superuser only creates roles and databases (`sandbox-server setup`);
  `ps_owner` owns the database and every object and is used only by
  `sandbox-server migrate`; the server logs in as `ps_app`, a member of
  `ps_runtime` with `SELECT, INSERT` on the append-only tables and
  `SELECT, INSERT, UPDATE` on mutable ones, no `TRUNCATE`, no DDL, no
  `TEMP`, `NOSUPERUSER NOBYPASSRLS`. The server checks its role at startup
  and refuses to run as a superuser, as a role with BYPASSRLS, as the
  journal's owner, or with `TEMP` (`sandbox_db::roles::check_runtime`).
  Tests: `tests/roles.rs`.

- **D-039 — Trigger functions name their tables and pin their search
  path.** Review finding: the balance and late-line triggers named
  `journal_lines` and `journal_entries` without a schema, and PostgreSQL
  searches the session's `pg_temp` first, so a role with only `INSERT`
  and `TEMP` shadowed them with temporary tables and committed a single
  +1,000,000 line and a late line on a posted entry. Every relation in
  every trigger function is now schema-qualified, every function pins
  `search_path = pg_catalog, pg_temp` (temp last, migration 0006), and
  the runtime role has no `TEMP` (D-038). Test:
  `a_temporary_table_cannot_shadow_the_journal_in_its_triggers`, run as a
  role that does have `TEMP`.

- **D-040 — A network conflict is final; a record is applied only to its
  own request.** Review finding: after a restore, the same key with a
  different amount was reinterpreted. The network's `409` became
  `uncertain`, and the readback's outcome was then applied to the server's
  new amount. Now the network's answers and readbacks carry the recorded
  request's fingerprint, kind and amount. A `409`, or a record whose
  fingerprint differs from the request the server would send, resolves the
  operation as declined (`network_identity_conflict`), releases its
  reservation, posts nothing, and answers the client
  `409 idempotency_key_conflict`. It is never retried and never
  reinterpreted. Test: `after_a_restore_the_same_key_with_another_amount_is_a_conflict_not_a_reinterpretation`
  (the reviewer's R1). The report flags the resulting server/network
  disagreement from D-041 on.

- **D-041 — Agreement with the network: per project, to completion, both
  ways, field by field.** Review finding: reverse reconciliation paged
  through *every* project's network operations, stopped after 100 pages
  without saying so, checked presence only, and only in one direction. It
  reported clean with 100,000 other-project records in front of the one
  that mattered. Now the simulator keeps a per-project index and lists by
  project. The server reads the project's pages to the end, or up to
  `PS_RECONCILE_NETWORK_MAX_PAGES` (1,000 pages of 1,000), and reports
  `complete: false` when it stops early; an incomplete scan is never
  clean. Every network operation must be recorded by the server with the
  same kind, amount and outcome, and every operation the server says the
  network answered must be on the network. Disagreements are listed (at
  most 1,000 each), never applied. This replaces the reverse-only check in
  D-036. Tests: `network_agreement_is_project_scoped_complete_and_checked_both_ways`
  (100,001 records) and the extended R1 test.

- **D-042 — An explanation is of one horizon.** Review finding: explain
  paged by `journal_entries.seq`, which is allocation order, not commit
  order, and each page read its own snapshot. A posting that took a lower
  sequence number but committed later was skipped, and the lines summed to
  −150 against a balance of −350. Now an explanation is of the first
  page's snapshot: only entries whose transactions that snapshot saw as
  committed count (`pg_visible_in_snapshot`). A transaction in progress at
  that moment stays out even after it commits, so the set can never change.
  Paging by sequence over it is stable, and the balance (the live balance
  at that moment) and every page are computed over that same set. The
  snapshot's `xmin` alone was tried first and rejected: it is cluster-wide,
  so any open transaction anywhere held the explanation back. The cursor
  `next` carries `as_of`, `after_seq` and the running total, so no page
  re-sums earlier lines (review 15). A newer explanation sees what committed since. Test:
  `explain_pages_stay_consistent_while_entries_commit_out_of_order`.

- **D-043 — The card guard normalizes before it looks.** Review finding:
  only ASCII digits joined by one space or dash, in six groupings, were
  detected. `4242.4242.4242.4242`, double spaces, NBSP, full-width digits,
  one-digit groups, `/`, `_` and a stray-space Amex number were all stored.
  Now digits (ASCII or full-width) are joined across separators of at most
  two non-alphanumeric characters, and every 13–19 digit window of a run is
  Luhn-checked. Letters, longer gaps and canonical UUIDs end a run, so UUID
  keys and ordinary dates pass. This refines D-019's detection rule; the
  check applies to every JSON string and key and to the `Idempotency-Key`.
  Test: `every_common_card_number_shape_is_refused_and_never_stored`.

- **D-044 — No error message carries client input.** Review finding: a
  card number sent as a JSON number came back in a `422 invalid_body`
  message, which serde wrote as "invalid type: integer `4242…`". Every
  JSON number is now card-guarded like a string, and schema errors have
  one fixed message that names no value. Test:
  `card_numbers_as_json_numbers_are_refused_and_schema_errors_never_echo_input`.

- **D-045 — Client request ids are UUIDs; logs carry route templates.**
  Review finding: a card number in the URL path or in `X-Request-Id` was
  logged (once at WARN, which the default filter emits) and the request id
  was echoed on the response. Now only canonical UUIDs are accepted as
  client request ids (anything else is replaced by a fresh UUIDv7), and
  the root span records the matched route template, never the concrete
  path. UUID path parameters are parsed by the server's own extractor:
  axum's `Path<Uuid>` logs its rejection, at TRACE, with the raw segment.
  `axum::rejection` logging is off in every filter. Test: `tests/logs.rs`
  (a card number in the path and in `X-Request-Id`).

- **D-046 — Loopback is not a credential: the local processes' internal
  routes need one.** Review finding: the simulator's executions, fences,
  listings, faults and virtual clock, and the development store's
  `PUT /config` (which swaps the API key and signing secrets it trusts),
  answered anyone who reached the port, including a webhook target the
  server was told to call; and the network decided a capture, void or
  refund against any parent authorization, whatever its project. Now the
  simulator refuses to start without `PS_SIM_TOKEN` (at least 32 visible
  characters) and answers `401` on executions, readbacks (and so fences)
  and listings without it; the server presents it (`PS_SIM_TOKEN`, a
  required setting, sent as a sensitive header). Test controls (the
  network's faults and clock, the store's `PUT /config`) are a control
  plane: mounted only when `PS_SIM_CONTROL` / `PS_STORE_CONTROL` is `on`,
  which by default only `PS_PROFILE=dev` is (`ci` and the default `shared`
  leave them off, so the routes do not exist), and then behind a separate
  operator credential (`PS_SIM_CONTROL_TOKEN`, `PS_STORE_CONTROL_TOKEN`)
  that must differ from the network's. An unknown profile, a control plane
  without a credential, or a weak credential refuses to start. Only digests
  are kept and compared. The network now declines a child operation whose
  parent authorization belongs to another project as `no_authorization`,
  exactly as an unknown one, so neither capacity nor existence leaks
  across projects. Webhook targets on the processes' own ports are not
  refused separately: a delivery carries no credential, so it gets `401` or
  `404`. `scripts/stack.sh env` prints public development credentials;
  `scripts/load.sh` makes fresh ones per run. Tests:
  `crates/sandbox-sim/tests/access.rs` and `fixtures/dev-store/tests/control.rs`
  (the real binaries), `another_projects_authorization_cannot_be_captured_voided_or_refunded`,
  `access::tests`.

- **D-047 — Reports are admitted by budget; the network's reads leave its
  writer.** Review finding: any `read` key could run unbounded-cost reports
  (`/v1/reconciliation` holds a repeatable-read scan; `?network=true` pages
  the network's listing), and every listing ran inside the simulator's
  single writer batch, so one project's report delayed every project's
  executions and readbacks. Now each project has a report budget, a token
  bucket of `PS_REPORT_BURST` units (default 600) refilled at
  `PS_REPORT_REFILL_PER_MINUTE` (default 600): an explain page costs 1, a
  reconciliation 10, one with network agreement 50; past it the answer is
  `429 rate_limited` with `Retry-After`. A project runs at most
  `PS_REPORT_PER_PROJECT` reports at once (default 2) and the server at
  most `PS_REPORT_CONCURRENCY` (default 8, `503 overloaded` past it), apart
  from the hot path's `/v1` gate. The project map is bounded (65,536); idle
  full buckets are evicted first, since a new bucket starts full. Hot-path
  routes never spend the budget. In the simulator, listings, clock reads
  and counts run in their own redb read transactions on blocking threads
  (at most 4 at once, a 2 s wait before `overloaded`), over the last
  committed state; they never enter the writer's queue. Load runs lift the
  report budget (`scripts/load.sh`), so a one-project explain scenario
  measures explain rather than the limiter. Tests:
  `tests/reports.rs` (`reports_are_rate_limited_per_project`),
  `reports::tests`, and the simulator's
  `listings_and_the_clock_are_served_while_the_writer_is_busy` (a test hook
  holds the writer; the listing used to wait behind it).

- **D-048 — The reconciler takes projects in turn, checks concurrently and
  backs off failures.** Review finding: each sweep took the 200 oldest open
  operations of *every* project and read them back one at a time, and an
  operation whose readback kept failing was picked first again on every
  sweep, so one project's backlog starved every other project's recovery
  while their reservations stayed held. Now a sweep takes the projects with
  open operations in turn (every project's most overdue operation, then
  every project's second, ...), at most `PS_RECONCILE_PER_PROJECT` of one
  project (default: the batch) and `PS_RECONCILE_BATCH` in all; readbacks
  run concurrently, at most `PS_RECONCILE_CONCURRENCY` (default 8); a check
  that fails postpones that operation (`operations.next_check_at`, one
  sweep interval doubling to `PS_RECONCILE_BACKOFF_MAX_MS`, default 5 min;
  `checks` counts them, migration 0007); a full batch is followed at once by
  the next sweep; and every sweep reports the oldest open operation's age,
  logging WARN past `PS_RECONCILE_ALERT_AGE_MS` (default 10 min). Tests:
  `tests/reconciler.rs`.

- **D-049 — One endpoint cannot hold the deliverer: per-endpoint caps, fair
  claims, streamed completions and a circuit breaker.** Review finding: one
  global claim order and a batch barrier (`run_once` waited for its whole
  batch) let one project's black-holed endpoint hold all 16 places for its
  5 s deadline, so every project dropped to about three deliveries a
  second. Now at most 4 attempts are in flight to one endpoint
  (`deliverer::PER_ENDPOINT`), claims take the endpoints in turn (every
  endpoint's most overdue delivery, then every endpoint's second, ...), and
  the running deliverer refills a place as soon as an attempt ends. Five
  failures in a row open an endpoint's circuit (`webhook_endpoints.
  consecutive_failures`, `circuit_opens`, `circuit_open_until`, migration
  0008): its deliveries are not claimed, and no attempts are spent, for
  30 s doubling with each opening up to 10 minutes; then one probe at a
  time; a success closes it. The breaker is updated in the same statement
  that records the attempt, only by the current lease holder, and from the
  endpoint row itself, so concurrent failures open it once. The caps count
  one deliverer process's attempts (one server in the local/CI mode,
  D-023). `GET /v1/webhook_endpoints` shows `consecutive_failures` and
  `circuit_open_until`. Tests:
  `a_black_holed_endpoint_cannot_hold_the_deliverers_capacity`,
  `an_endpoint_that_keeps_failing_opens_its_circuit_and_is_probed_once`.

- **D-050 — An explanation cursor is valid only in its restore generation.**
  Review finding: after a restore, the journal's sequence restarts at the
  backup's value, so the numbers after the backup point are handed out
  again to other entries, and the replayed requests re-post that history
  with new `posted_at` values (entry ids and amounts are re-derived
  identically). A cursor taken before the restore would page a different
  history. Now every explanation names the database's restore generation
  (the cluster's system identifier, its timeline and the database's OID:
  a logical restore makes a new database, a promoted physical restore a
  new timeline, another cluster another identifier), its `next` cursor
  carries it, and a cursor from another generation is refused with
  `409 stale_cursor`, never continued. A cursor must now pass `generation`
  with `as_of`, `after_seq` and `running`. Restore semantics, stated
  plainly: history up to the backup is restored byte for byte (D-026, the
  rehearsal in D-053); what happened after it is reconciled from the
  network's newer record (D-015) and re-posted under new sequence numbers.
  Test: `an_explanation_cursor_from_before_a_restore_is_refused`.

- **D-051 — Background tasks run supervised and readiness reports them.**
  Review finding: the reconciler and the deliverer were plain
  `tokio::spawn`, so a panic ended them silently: the `JoinError` was
  ignored until shutdown, nothing restarted them, and `/readyz` stayed
  green while uncertain operations and webhooks waited forever. Now each
  runs under `supervisor::supervise`: a panic, or a return while the server
  is not stopping, is logged at ERROR with the task's name and restarted
  after 1 s, doubling to 30 s (reset after a minute of healthy running);
  nothing is restarted on shutdown. Each task beats after every pass (a
  sweep; a delivery-loop turn), and `/readyz` is `503` with
  `reason: background_task` while a task is down between a failure and its
  restart, or when its last beat is older than its bound (the reconciler:
  ten intervals plus two minutes; the deliverer: two minutes), so a stuck
  task is reported as well as a dead one. `/readyz` lists every task's
  `running`, `restarts`, `last_failure` and time since its last beat.
  Tests: `supervisor::tests` (paused time) and the drill
  `a_panicking_reconciler_is_restarted_and_still_resolves_uncertain_operations`,
  whose real binary panics in its first sweep (`PS_FAULT=reconciler_panic`,
  fault-injection builds only).

- **D-052 — One clock per timestamp in delivery.** Review finding: a new
  delivery's `next_attempt_at` defaulted to the database's `now()`, while
  claims compare it with the deliverer's injected clock, so a deliverer
  clock behind the database's stalled every new delivery until it caught
  up (a delay, not a loss). Now every timestamp is compared only on the
  clock that wrote it: a new delivery is due at `-infinity`, at once on
  any clock (migration 0009); retries, leases and the breaker (D-049) are
  written by the deliverer's clock and compared on it; a rotation window
  (`previous_expires_at`, set by the API) is written by the database's
  clock and now compared on it in claims too, as the endpoint listing
  already did. Test:
  `a_new_delivery_is_due_at_once_whatever_the_deliverers_clock_says`.

- **D-053 — The logical restore is rehearsed by a script and a test.** The
  review restored faithfully by hand; D-026 owed the scripted rehearsal.
  `scripts/restore-rehearsal.sh <source> <new>` takes a `pg_dump` custom
  backup of a live database, creates a fresh database the way `setup`
  does (owner `ps_owner`, only `ps_runtime` may connect, no `TEMP`),
  restores it with `pg_restore --single-transaction --exit-on-error`, and
  fails unless a fingerprint of both is identical (every table's row
  count; digests of every journal entry, line and operation; every
  trigger, enabled, with its function; every function's pinned
  `search_path` and owner; every object's owner; the runtime role's
  grants; every sequence's position) and the copy's invariants hold (every
  entry balances per currency, none lacks lines, no trigger is disabled,
  the journal has its triggers, the runtime role can neither `TRUNCATE`
  nor create temporary tables). The tools run inside the PostgreSQL
  container (`PS_PG_CONTAINER`; CI passes its service container), so their
  version matches the server's. Test:
  `a_logical_restore_is_the_same_ledger_with_the_same_protections` — a
  ledger with captures, a refund, a void and a decline, backed up while the
  server's pool is open; on the copy the role check passes, the
  reconciliation report, balances and explanation are the same, the
  generation differs (D-050), the owner's `UPDATE` of a line is refused
  (`PSJ01`), and a new posting takes a sequence number after every
  restored one.

- **D-054 — Every listener is bounded at the transport.** Review N1: the
  server, the simulator and the development store used `axum::serve`,
  which builds hyper's connection without a timer, so the header-read
  timeout never ran; nothing bounded idle keep-alive connections, the
  number of connections, or a client that stops reading, and the store had
  no request timeout. Half-sent heads and idle connections stayed open
  past 40 s, and under a 64-descriptor limit 70 silent connections stopped
  the simulator accepting its real client. Now all three serve through
  `sandbox-http`: hyper's HTTP/1 builder with `TokioTimer`; a connection
  cap taken *before* accept (past it, connections wait in the listen
  backlog and use no descriptor); a read timeout for request heads (hyper
  restarts it while a keep-alive connection waits for its next request, so
  it is also the idle timeout) and for pauses inside a request body; a
  write deadline that closes a connection whose response write makes no
  progress; and, per connection, a request count and an age past which the
  next response carries `Connection: close`. Shutdown stops accepting,
  asks every connection to close after its request and cuts stragglers off
  after the grace period. The store also gets a 10 s request timeout and a
  64 KiB body limit. Settings: `PS_`, `PS_SIM_` and `PS_STORE_` +
  `MAX_CONNECTIONS` (512 / 1,024 / 256), `READ_TIMEOUT_MS` and
  `WRITE_TIMEOUT_MS` (10 s), `MAX_REQUESTS_PER_CONNECTION` (10,000) and
  `MAX_CONNECTION_AGE_MS` (10 min), all chosen, not measured: the owed
  load runs set them. A client must keep pooled connections idle for less
  than the server's read timeout, or it may send on a connection being
  closed (for an execute, that would be an honest but needless
  `uncertain`); the sandbox's own clients keep theirs at most 4 s
  (`sandbox_http::CLIENT_POOL_IDLE`). The cap should sit below the
  process's descriptor limit. Tests: `tests/transport.rs` in the server
  (half-sent, idle and silent connections closed; request and age limits;
  the cap holds a fifth connection in the backlog), the simulator (70
  silent connections under `ulimit -n 64`; a half-sent head) and the store
  (a stalled webhook body and a half-sent head), each against the real
  binary; `sandbox-http`'s write-stall test over a 1 KiB pipe.

- **D-055 — The journal does not trust a transaction id across a restore.**
  Review N2: `created_xact` is a cluster-local transaction id. A `pg_dump`
  restored into another cluster kept the old ids, so explain's snapshot
  filter treated every restored entry as not yet committed (0 lines and a
  0 balance while `/v1/balances` showed 600 and reconciliation was clean),
  and once the new cluster's counter reached a restored entry's
  `created_xact`, the runtime role added lines to that posted entry. Now an
  entry is *native* when its row's `xmin`, which always belongs to the
  current cluster because a restore inserts every row anew, equals the
  posting transaction's id (`xmin = created_xact::xid`); the stamp trigger
  guarantees this for every entry posted here outside savepoints (a
  posting inside a savepoint is refused, fail closed). A line may join an
  entry only if the entry is native and posted by this very transaction
  (migration 0010); explanations count a non-native entry as committed
  before every snapshot. Freezing keeps the raw `xmin`, so old native
  entries stay native, and a 32-bit wrap cannot reopen one: the 64-bit
  `created_xact` must still equal the current transaction's id. No
  per-posting cost beyond reading a system column. The local stack and CI
  run a second, empty cluster (`postgres-restore`, 127.0.0.1:56450, its own
  named volume); `scripts/restore-rehearsal.sh` restores into it when
  `PS_PG_TARGET_CONTAINER` names it. Test:
  `a_restore_into_another_cluster_explains_the_same_and_keeps_posted_entries_closed`
  — every balance explains to the source's lines on the copy, then the
  copy's counter is walked to each of three restored entries' ids and, at
  each, `ps_app` tries to add a balanced pair of lines to that entry:
  refused.

- **D-056 — Explanation cursors are sealed by the server; query strings are
  card-guarded.** Review N3: the cursor's `as_of` went straight into a
  `::pg_snapshot` cast, so a card number there became a 500 whose message
  quoted it in the server's ERROR log and in the database's log, and
  `running` was whatever the client sent. Now `next` is `{"cursor": …}`:
  the fields (generation, snapshot, sequence, running total) and an
  HMAC-SHA256 tag (128 bits) over them, written with the letters `a`–`p`, so
  a cursor has no digits for the card guard to mistake. The server opens it
  before any query: shape, every field's exact grammar (the snapshot as
  `xmin:xmax:xip,…` with ascending ids inside the range), then the tag; a
  forged or garbled cursor is `400 invalid_cursor`, one of another restore
  generation `409 stale_cursor` (D-050), and any other query field is
  refused. The key is derived from `PS_SECRET_KEY` when set (cursors
  outlive a restart), otherwise random per process. Every query string is
  percent-decoded and card-guarded (`422 raw_card_data_rejected`), and a
  logged error cause that quotes anything card-like is withheld whole.
  Tests: `a_cursor_the_server_did_not_issue_is_refused_before_any_query`,
  `tests/logs.rs` (a card number in a cursor), `cursor::tests`.

- **D-057 — Delivery fairness is per project too, and endpoints are
  bounded.** Review N4: D-049's caps were per endpoint, and a project could
  create any number of endpoints, so one project's four black-holed
  endpoints took all 16 places and another project's first delivery waited
  3.67 s; the listing showed only the first 100. Now claims take the
  projects in turn, then each project's endpoints in turn, and one project
  holds at most 8 of the 16 places (`deliverer::PER_PROJECT`, chosen, not
  measured), on top of 4 per endpoint and one per half-open endpoint. A
  project has at most 16 active endpoints
  (`webhooks::MAX_ENDPOINTS_PER_PROJECT`; creations of one project are
  serialized by a transaction-scoped advisory lock, so two at once cannot
  both take the last place; disabling one frees its place), and
  `GET /v1/webhook_endpoints` pages (`limit` 1–100, `after`, `next_after`).
  Tests: `a_project_with_many_black_holed_endpoints_cannot_hold_the_deliverer`,
  `endpoints_are_capped_per_project_and_listed_in_pages`.

- **D-058 — The journal's protections are one manifest, checked absolutely.**
  Review N5: the rehearsal proved "the copy equals its source", so a source
  whose owner had reset a trigger function's search path, dropped an
  append-only trigger and granted the runtime role UPDATE produced
  `restore ok`, and `ps_app` then rewrote lines on the copy; the server's
  startup check looked at its role, not at the protections. Now
  `crates/sandbox-db/sql/protections.sql` states them once and returns one
  row per problem: each of the 16 triggers present and enabled, each of the
  5 functions present and every function in `public` pinning
  `search_path = pg_catalog, pg_temp`, `ps_runtime`'s exact privileges on
  every table (no more and no less than the migrations grant), `ps_owner`
  owning every table, and no `TEMP` or `CREATE` for the runtime role. The
  server refuses to start on any problem (`check_runtime`,
  `RoleProblem::Unprotected`); the rehearsal runs it on the source before
  the dump and on the copy after it, and on the copy also requires that
  `ps_app`'s `UPDATE` of a journal line and `DELETE` of an entry are
  refused. A migration that changes any protection must change the
  manifest in the same commit. (D-055 made the "counter above every
  restored id" precondition unnecessary, so it is not checked.) Tests:
  `the_server_refuses_a_database_whose_protections_were_weakened` (five
  weakenings, each alone), `the_rehearsal_fails_when_the_source_itself_was_weakened`.

- **D-059 — The second review's low findings.** Each closed with a test that
  failed first.
  - *N6:* a set-but-empty `PS_SIM_TOKEN` is refused like a weak one (it
    used to pass, leaving every operation uncertain while readiness was
    green). Test: `config::tests::the_network_credential_is_required_and_strong`.
  - *N7:* the card guard joins digits across separators of up to three
    characters (`4242 - 4242`), reads every Unicode decimal digit (a table
    of the `Nd` blocks' zeros, checked against `char::is_numeric`) plus
    superscript, subscript and circled digits, and exempts only UUIDs that
    contain a letter, so the all-digit UUID shape is digits like any other
    and is no longer accepted as a request id either. Tests:
    `wide_gaps_other_digit_forms_and_all_digit_uuids_are_detected`,
    `every_digit_block_is_ten_numeric_characters`, `request_id_policy`,
    `every_common_card_number_shape_is_refused_and_never_stored`.
  - *N8:* `GENERATED ALWAYS AS IDENTITY` still allows `OVERRIDING SYSTEM
    VALUE`, so the runtime role stored an entry at `seq = -5` (never listed
    by explain, counted in the balance). The stamp trigger now takes the
    sequence's next value whatever the caller wrote (migration 0011).
    Test: `the_runtime_role_cannot_choose_an_entrys_sequence_number`.
  - *N9:* the network refuses an operation whose amount is outside
    1..=10^12 (`422`, nothing recorded) before deciding; a capture of −500
    used to be approved and lower its record of what was captured, after
    which 1,500 was captured against 1,000 authorized. Test:
    `amounts_outside_the_valid_range_are_refused_and_move_nothing`.

## Open questions (still open)

- **Dashboard authentication** — per-project-owner session login is the
  default assumption; decided with the dashboard milestone.
- **Billing scenario order and definitions** — the default order is
  settlement → fees → payouts → disputes → invoices → recurring, because
  each later scenario reads settled balances; each gets its own contract
  when reached.
- **Hosted mode** — quotas per project, the private-range egress guard and
  sign-up abuse limits, if a hosted instance is ever offered.
