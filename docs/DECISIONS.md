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
  committed state; they never enter the writer's queue. Every number here
  (600/600, 2, 8, the costs, 4 readers, 2 s) is chosen, not measured: the
  owed load runs set them (review N13). Load runs lift the
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
  logging WARN past `PS_RECONCILE_ALERT_AGE_MS` (default 10 min). These
  defaults (8, 5 min, 10 min) are chosen, not measured; the owed load runs
  set them (review N13). Tests:
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
  30 s doubling with each opening up to 10 minutes (4, 16, 5 failures,
  30 s and 10 min are chosen, not measured; the owed load runs set them,
  review N13); then one probe at a
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
  backup of a *quiescent* database (the fingerprint of the source is taken
  after the restore, so a write in between fails the comparison; review
  N13), creates a fresh database the way `setup`
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
  server's pool is open but idle; on the copy the role check passes, the
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
  refused. Not supported: a logical-replication subscriber as the server's
  database. Its apply worker writes replicated rows in its own transactions,
  so they are not native there and count as committed before every
  snapshot; an entry applied after an explanation began, with a lower
  sequence number than a page already read, would be skipped, so
  explanations across replicated entries would not be stable. Restore by
  `pg_dump`/`pg_restore` (or a physical copy) instead.

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
  A second review check found cursors unbound: project B accepted project
  A's cursor and carried on with A's running total. A cursor now seals what
  it explains (project, dataset epoch, currency, account) and opens only
  for that (`400 invalid_cursor` otherwise).
  Tests: `a_cursor_the_server_did_not_issue_is_refused_before_any_query`,
  `a_cursor_explains_only_the_account_it_was_issued_for`,
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
  manifest in the same commit. A second review check showed names were not
  enough: as the owner, a trigger function replaced by a no-op (search path
  still pinned) passed, the server started, the rehearsal said `restore ok`,
  and `ps_app` posted an unbalanced late line. So the manifest also pins
  every trigger's definition (`md5(pg_get_triggerdef)`: events, timing,
  level, deferral, function) and every function's (`md5(pg_get_functiondef)`:
  body and settings), lists every function allowed in `public`, and requires
  each to be SECURITY INVOKER and owned by `ps_owner`; a migration that
  changes one updates its digest in the same commit. Tests also:
  `the_server_refuses_replaced_function_bodies_and_trigger_definitions`,
  `the_rehearsal_fails_when_a_function_body_was_replaced`. (D-055 made the "counter above every
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
  - *N10:* webhooks may never target the server's listen port, the
    network's port or PostgreSQL's (whatever the host form, IPv4-mapped
    included), at registration (`422`) and again at delivery (`dead`).
    Any other loopback port needs `PS_WEBHOOK_PORTS` (`9,56100-56199`), or
    the developer's opt-in `PS_PROFILE=dev` when that is unset; with
    neither, no target is accepted. (This refines D-046, which relied on the
    credentials alone.) `scripts/load.sh` lists the store's port. Test:
    `webhooks_may_not_target_the_sandboxs_own_ports_or_unlisted_ones`.
  - *N11:* the development store's `GET /fulfillments` pages (`limit`
    1–1,000, `after`, `next_after`) and `/stats` counts the table instead
    of reading it (it read about 100 MB per call after a million
    deliveries). Test: `fulfillments_are_listed_in_pages`.
  - *N12:* `explain_pages_stay_consistent_while_entries_commit_out_of_order`
    pages by two lines and requires at least two pages (with one page it
    passed even without D-042's fix; with the snapshot filter removed it
    now fails, "the balance moved between pages"). A `TestDb` dropped
    without `cleanup` (a failing test) drops its database from a thread of
    its own, and the first test database of each process sweeps what
    killed processes leaked: `ps_test_*` databases older than two hours
    and `ps_shadow_*` roles older than one, by the UUIDv7 time in their
    names. Tests: `a_test_database_is_dropped_even_when_its_test_never_cleans_up`,
    `stale_test_databases_and_roles_are_swept`.
  - *N13:* every budget, breaker, reconciler, reader and transport default
    is labelled "chosen, not measured" where it is stated (D-047, D-048,
    D-049, D-054, D-057); the owed load runs set them. The rehearsal needs
    a quiescent source (D-053 and the script said the server "may keep
    running", but the source's fingerprint is taken after the restore).

## M6: the checkout and the dashboard (2026-11-17)

M6's row names a checkout (scenario picker, no card field) and an inspection dashboard
on separate origins, and its gate the first useful release end to end. These fix the
smallest version that meets that gate.

- **D-060 — Checkout sessions: the merchant fixes the price, the payer picks only a
  scenario, and a session can never charge twice.** A merchant (`payments:write`) creates
  `POST /v1/checkout_sessions {amount, currency, description?, capture_method?
  (automatic | manual), embed_origins?, return_url?, expires_in?}`; the answer carries the
  session, its client secret (`cs_sim_` + 64 letters `a`–`p` from 32 random bytes, shown
  once, only its SHA-256 stored, no digits for the card guard to mistake) and the payer's
  URL `{PS_CHECKOUT_ORIGIN}/c/{id}#{secret}`: the secret rides in the fragment, which
  browsers never send, log or put in a `Referer`. Descriptions refuse control,
  bidirectional and invisible characters; embed origins are exact loopback origins (at
  most 4, D-023); a return URL is http(s) on a loopback host; a session lives 60 s–24 h
  (default 1 h) and allows at most 10 attempts. Every "Pay" is an attempt committed under
  the session's row lock *before* any payment work, with the payer's `Idempotency-Key`
  (one UUID per attempt, kept in the tab) and the scenario; its authorize key
  (`checkout.<session letters>.<n>`) and so its operation and payment ids (D-015) are
  derived, so the identity exists before the click reaches the network. A new attempt is
  admitted only when there is none or the latest ended declined or never executed; while
  one is pending, uncertain or succeeded another key gets `409 checkout_attempt_open`, so a
  double click, a reload or a retry with a fresh key cannot start a second charge. The same
  key re-drives its own attempt (another scenario under it is `409
  idempotency_key_conflict`). With automatic capture the attempt's capture (key
  `…<n>.capture.<try>`) runs once the authorization succeeded; a try the network never
  executed is retried under the next try's key, at most 3. `POST …/continue` re-drives the
  latest attempt without the tab's key, so a page finishes after the reconciler resolved
  an uncertain answer or after a server crash. What the payer is told is derived from the
  recorded operations and the payment's journal-backed sums
  (`sandbox_domain::checkout::derive`): `open`, `processing`, `uncertain`, `declined`,
  `not_executed`, `authorized`, `paid` — `paid` only when the whole amount was captured,
  never from an uncertain answer. Cost: an abandoned tab's capture waits for the merchant
  (nothing captures in the background); the merchant can capture or void through the API.
  Tests: `tests/checkout.rs` (9), `checkout::tests` in the domain (8), and the kill drill
  through the checkout (`drill_a_through_the_checkout_*`, the real binary).

- **D-061 — Two more listeners; their pages served from memory under hash-pinned
  policies.** The server binds the checkout (`PS_CHECKOUT_LISTEN`, default
  127.0.0.1:56083) and the dashboard (`PS_DASHBOARD_LISTEN`, 56084) when their builds are
  configured (`PS_CHECKOUT_DIR`, `PS_DASHBOARD_DIR`); each is its own origin
  (`PS_*_ORIGIN`, loopback, the two must differ) with its own router, the same transport
  limits (D-054) and the `/v1` shedding gate; neither serves `/v1`, and webhooks may not
  target either port. A build is read into memory at startup (at most 512 files and 16
  MiB, no symbolic links, a missing `index.html` fails startup), so no request touches the
  file system. The SHA-256 of every inline script of `index.html` goes into the page
  policy: `default-src 'none'; script-src 'self' 'sha256-…'; style-src 'self'; img-src
  'self'; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none';
  form-action 'none'; object-src 'none'; frame-ancestors …` — no `'unsafe-inline'` for
  scripts or styles. SvelteKit's route announcer is one static `style="…"` attribute in the
  app's own script; the site hashes every such attribute it finds in its scripts and allows
  exactly those values under `'unsafe-hashes'` (which admits only listed values, never an
  arbitrary inline style); mode-watcher's transition-blocking `<style>` is switched off and
  its head script is static in `app.html`, so it is one of the hashed scripts. Sizes the
  apps set at run time go through the CSSOM, which the policy does not govern. The
  dashboard is never framed (`frame-ancestors 'none'`,
  `X-Frame-Options: DENY`). The checkout is embeddable only where its session says:
  `frame-ancestors` lists the session's `embed_origins` and is `'none'` (with
  `X-Frame-Options: DENY`) otherwise; an embedded page tells its parent the outcome with
  `postMessage` addressed to each allowed origin, never `*`. Every response also carries
  `nosniff`, `Referrer-Policy: no-referrer`, `Cross-Origin-Opener-Policy` and
  `Cross-Origin-Resource-Policy: same-origin`, a `Permissions-Policy` that turns off
  camera, microphone, geolocation, payment and USB, `Cache-Control: no-store` (hashed
  assets: a year, immutable) and `Sandbox-Simulated: true`. Tests:
  `the_page_is_framed_only_by_its_sessions_origins_under_a_hashed_policy`,
  `the_dashboard_is_never_framed_and_its_scripts_are_pinned_by_hash`, `site::tests`.

- **D-062 — Dashboard sign-in: a sandbox key exchanged for a host-only session; CSRF by
  origin and a session token.** Closes the open question "Dashboard authentication". A
  person signs in by presenting a project's `sk_sim_` key (`POST /api/session`, as
  `Authorization: Bearer`; any scope, since the dashboard only reads money). The server
  keeps the session in memory (at most 4,096; 30 minutes idle, 12 hours at most; expired
  ones are swept when the table is full, then `503`), holding only the key's hash, which it
  re-resolves on every request, so a revoked key ends its sessions at once. The cookie is
  `__Host-ps_dashboard` (32 random bytes as letters; `Secure; HttpOnly; SameSite=Strict;
  Path=/`, no `Domain`), which Chromium keeps on `http://127.0.0.1` because loopback
  counts as secure (checked with Chromium 156; Safari does not, so it cannot sign in over
  plain HTTP). Cookies are not isolated by port, so the checkout origin on the same host
  would send this cookie and could plant one. Hence: a sign-in ends any session the
  browser presented (no fixation); a doubled cookie is trusted by neither copy; every
  dashboard API request a browser says another origin started (`Sec-Fetch-Site` other than
  `same-origin`) is refused, so a sibling origin cannot even make it run a query; and every
  state change (sign-in, sign-out, simulator controls) needs `Origin` equal to the
  dashboard's own and, once signed in, `X-CSRF-Token` equal to the session's (constant-time
  compared). The reads are the API's own handlers mounted read-only under `/api/v1`: the
  session hands them its identity in the request's extensions and the request's own
  `Authorization` is dropped, so a key is no credential on this origin. The checkout keeps
  no cookie at all; its POSTs need the same `Origin` and fetch-metadata checks. A restart
  signs everyone out. Tests: `tests/dashboard.rs` (5) and `dashboard::tests` (3).

- **D-063 — Simulator controls in the dashboard: the dev profile only, the operator's own
  credential, forwarded and never kept.** The dashboard's fault rules and virtual clock
  exist only when the server runs with `PS_PROFILE=dev` (in `ci` and `shared` the routes
  are not mounted). Each request carries the operator credential the person typed
  (`X-Operator-Token`, held in the tab's memory only) plus the session's CSRF token; the
  server forwards the credential to the network's control plane as its `Authorization`
  over a client with no credential of its own (2 s deadline, 64 KiB answers), and the
  network decides (D-046). A dashboard session alone cannot change the network. Test:
  `simulator_controls_need_the_dev_profile_and_the_operators_own_credential`.

- **D-064 — The web apps' budgets.** Each app's first visit is measured on the built output
  (`scripts/web-budget.mjs`, gzip level 9): the JavaScript `index.html` loads before the app
  can run, all JavaScript, and the stylesheet; fonts load per unicode range on demand and
  are counted apart. Budgets, about 20 % over the first measurement (checkout 74.0 / 81.1 /
  8.4 KB, dashboard 82.8 / 100.7 / 8.8 KB): checkout 90 / 100 / 11 KB, dashboard 100 / 125 /
  11 KB; a miss fails `scripts/check.sh` and CI, and raising one needs a measurement and an
  entry here. A list that can grow past a few hundred rows (payments, an explanation's
  lines) renders only the rows in view (`VirtualRows`, fixed row height); the payments list
  pages by `next_before` (keyset on `(created_at, id)`, the `payments_recent` index), the
  explanation by its sealed cursor. Page loads are recorded by `scripts/web-perf.sh` in
  `perf/results/` with the machine, its load and the commit; they are loopback
  observations on a development machine, not capacity, and have no threshold yet: the
  first record sets the baseline.

- **D-065 — The M6 review: a restore cannot charge twice; the dashboard has a host of its
  own; sessions per key are bounded.** Each closed with a test that failed first.
  - *M1 (high):* after a restore of an older backup the server reuses an attempt number
    the network already answered. A different scenario under it got the network's `409`,
    which D-040 records as a decline (`network_identity_conflict`), and any decline admitted
    a new attempt: approve then charged a second time (two authorizations and two captures
    of 2,500). Now that conflict never counts as a decline: the attempt shows `conflict`
    ("Needs review"), the session takes no new attempt (`409 checkout_closed`), and
    reconciliation against the network reports the disagreement. Attempt keys keep their
    numbers on purpose, rather than gaining the restore generation (D-050): a number keeps
    its meaning across a restore, so the same scenario paid again under it reaches the
    network's record and recovers the payment once (D-015), whereas a key that changes with
    the generation would make the first attempt after a restore a new identity, a second
    real charge the conflict check could never see; and a session can never move past an
    attempt the network approved, because that number either replays the approval or
    conflicts. Tests: `after_a_restore_a_conflicting_attempt_closes_the_session_and_never_charges_twice`
    (a `pg_dump` restore into the second cluster, D-055) and
    `after_a_restore_paying_again_recovers_the_payment_without_a_second_charge`;
    `checkout::tests::a_network_identity_conflict_closes_the_session_instead_of_declining`.
  - *M2 (low):* browsers send a cookie to every port of its host, and a dashboard request
    carrying the cookie but no fetch metadata was answered. The dashboard now has a host
    name of its own, `http://dashboard.localhost:56084` by default (`*.localhost` is
    loopback, RFC 6761, and Chromium keeps `__Host-` cookies there; checked with Chromium
    156): a request on any other host is redirected there (pages) or refused `421`
    (anything else), so its cookie is never set for, or sent to, 127.0.0.1. Every API read
    also needs `Sec-Fetch-Site: same-origin` or, without fetch metadata, the dashboard's
    exact `Origin`; scripts and `curl` use `/v1` with a key instead. Embed origins and
    return URLs accept `*.localhost` names too. Tests:
    `reads_need_fetch_metadata_or_this_origin_even_with_the_cookie`,
    `the_dashboard_answers_only_on_its_own_host_name`, and the sign-in journey (the cookie's
    domain is the dashboard's host; the checkout's origin has no such cookie).
  - *M3 (low):* one key could fill all 4,096 sessions and lock everyone out of sign-in for
    30 minutes. A key now holds at most 8 sessions; its next sign-in ends its oldest.
    Filling the table takes 512 keys. Test:
    `one_key_holds_a_bounded_number_of_sessions_and_its_oldest_go_first`.

## M7: the developer product (2026-11-17)

M7's row names a Rust CLI, a TypeScript SDK, a seeded scenario library, dataset reset as a
new epoch, sandbox labels end to end and migration/restore tests per release; its gate: each
library scenario run twice yields the same states and events, reset needs `admin` and the
typed slug and never deletes journal rows, and production-looking SDK configuration is
refused. These fix the smallest version that meets it.

- **D-066 — Dataset reset: one epoch forward, typed, raced safely, nothing deleted.**
  `POST /v1/project/reset {confirm, epoch}` needs the `admin` scope; `confirm` must be the
  project's slug exactly (`422 confirmation_mismatch` otherwise) and `epoch` the current
  one (`409 epoch_changed`, naming the current epoch, otherwise), so a repeated or raced
  request cannot reset twice: eight concurrent confirmations of one epoch open exactly one
  new epoch. One transaction locks the project row, moves `dataset_epoch` forward by one and
  appends a `dataset.reset` audit event naming the key. The runtime role gains exactly
  `UPDATE (dataset_epoch)` on `projects` (a column grant, which also lets it take the row
  lock); a trigger (`projects_epoch_moves_forward`, SQLSTATE `PSP01`) refuses every other
  change to a project, for the owner too, and projects can no longer be deleted or
  truncated (migration 0013). The protections manifest (D-058) gains the three triggers and
  the function, and now checks **column** grants: `has_table_privilege` does not see them,
  so `GRANT UPDATE (amount_minor) ON journal_lines TO ps_runtime` used to pass the server's
  startup check (shown failing first, then fixed). After a reset: current reads, balances,
  explanations and idempotency scopes use the new epoch (the same keys derive new
  identities, D-015); a payment of an earlier epoch answers `410 dataset_archived` rather
  than `404`, for reads and for captures, voids and refunds; explanation cursors of the old
  epoch are refused (they seal it, D-056); checkout sessions of the old epoch read as
  expired, so they take no new attempt; operations still open resolve later **in their own
  epoch** (the reconciler uses each operation's epoch), so their money never appears in the
  new one; events already owed are still delivered (at least once, D-006), and the
  development store records an archived payment's event and fulfills nothing (`410` is
  final, not a reason to retry). Nothing of any epoch is deleted or changed. The dashboard
  offers the same reset on a Dataset page (typed slug, the epoch shown in its header): the
  API's own handler behind the session, so `admin` is still required, plus the dashboard's
  Origin and CSRF token (D-062); a lost answer is shown as unknown, with a "check what
  happened" read-back, never as done. Tests: `tests/reset.rs` (5), the dashboard's
  `the_dashboards_reset_needs_admin_the_typed_slug_origin_and_csrf`, the webhook test
  `an_event_delivered_after_a_reset_is_recorded_and_fulfills_nothing`, the weakenings in
  `the_server_refuses_a_database_whose_protections_were_weakened`, and the journeys in
  `e2e/tests/dataset.spec.ts` (keyboard alone, a raced reset, a lost answer).

- **D-067 — The interface standard is tested in the journeys.** Maintainer standard,
  2026-11-17: every screen added or touched uses the shared kit, designs every state (empty
  with a next step, loading without layout jump, errors in plain words with a way to
  recover and the code behind a details toggle, success, honest unknown), works by
  keyboard with visible focus and labels, and reads at 360 and 1440 px in light and dark.
  The proof is in `e2e/`: `a11y.spec.ts` runs axe (`@axe-core/playwright`, WCAG 2.1 A and
  AA rules) on every screen of both sites in both schemes and fails on any serious or
  critical violation (its first run found the new header link distinguishable by colour
  alone, `link-in-text-block`; fixed), and journeys capture screenshots at 360 and 1440 px,
  light and dark, into `docs/screens/` when `scripts/screens.sh` runs them (CI writes
  none). Screens that existed before M7 and were not touched keep their M6 design; the
  axe check covers them all the same.

- **D-068 — Clients refuse production-looking configuration, retry only under the same
  key, and never call an uncertain answer done.** One set of rules for `sandbox-cli`
  (`crates/sandbox-cli`) and the TypeScript SDK (`packages/sdk-ts`). *Configuration:* a key
  that is not `sk_sim_` followed by 64 hex characters is refused outright (`sk_live_`,
  `sk_test_` and every other shape; the key is never echoed); a real processor's host
  (`stripe.com`, `adyen.com`, `paypal.com`, ... and their subdomains: a belt-and-braces list)
  is refused outright; any other host that is not loopback (`127.0.0.0/8`, `::1`,
  `localhost`, `*.localhost`) is refused unless the caller says so explicitly
  (`--allow-non-local` / `PS_ALLOW_NON_LOCAL=1` / `allowNonLocal: true`), for a CI service
  container or a shared host (D-071); URLs with credentials, a path, a query or a fragment
  are refused. Every answer must carry `Sandbox-Simulated: true`, or the client stops
  (`NotASandbox`). *Retries:* every money-moving request carries an `Idempotency-Key` (a
  fresh `cli_` + 32 letters `a`–`p` when the caller gives none: no digits, so the card
  guard can never mistake one), and every retry reuses it, so the server replays or
  re-drives the same identity (D-015, D-016) and a retry can never charge twice. Retried:
  no answer at all, `429`, and `503` `payment_busy`/`overloaded`/`unavailable`/
  `deadline_exceeded`; never a `409`, `422` or other refusal. At most 4 attempts, full
  jitter over 100 ms doubling to 2 s, at least the server's `Retry-After` (capped at 2 s),
  never past an overall deadline (30 s by default), each attempt bounded (10 s), answers
  read up to 4 MiB. A request without a key (a reset, a key) is re-sent only when no attempt
  can have reached the server. *Honest answers:* `202` with a `pending` or `uncertain`
  operation is returned as such (the CLI exits 4 and says "UNCERTAIN ... not done yet");
  a money-moving request with no usable answer after its retries is `Unknown`, naming the
  key to retry with ("never with a new key"), never "failed"; `wait` polls the payment
  until nothing of it is open and otherwise says what is still open. *The CLI:* `status`,
  `payments create|get|list|capture|void|refund|wait` (amounts in the currency's units,
  `42.00`, `500`, `1.250`), `ledger balances|journal|explain` (every page, checking the lines
  sum to the balance), `reconcile [--network]`, `events`, `scenarios list|show|run`, and
  `reset` (the slug typed at a prompt, or `--confirm <slug>`; never without it); plain words
  by default, `--json` for the API's JSON; exit status 0 done, 1 refused, 2 usage or
  configuration, 3 declined or never executed, 4 uncertain or unknown, 5 unreachable or not
  a sandbox; the notice first on standard error on every run. Tests: `config::tests`,
  `client::tests`, `tests/cli.rs` (the binary against a whole in-process sandbox over real
  sockets; a proxy that loses the first response proves the retry replays one payment; a
  fake sandbox proves which answers are retried, under one key, and the bound).

- **D-069 — The scenario library is data with committed traces, checked by running each
  scenario twice and again after a reset.** `fixtures/scenarios/<name>.json` holds eleven
  scenarios (a purchase; declines for insufficient funds, do-not-honor and an expired
  method, the last repeated; a lost reply resolved by readback; an unavailable network
  fenced as never executed; partial capture then void; refunds up to the capacity and one
  cent past it; idempotent repeats and a conflicting amount; capture over authorization;
  JPY and KWD) as numbered steps (`authorize` with a `tok_sim_*` scenario, `capture`,
  `void`, `refund`, `wait`, `repeat`), each step's key `scn_<run>_<scenario>_<step>`. A
  run's trace is every step's status, outcome, replay flag and payment status, and every
  payment's operations, events in sequence and journal entries in posting order; ids,
  keys, network references, attempt counts and times are left out, so two runs compare
  equal exactly when the sandbox behaved the same. The expected trace of each scenario is
  committed (`fixtures/scenarios/expected/`), reviewed by hand when first written
  (`PS_BLESS_SCENARIOS=1` writes them), and compiled into the CLI: a release that changes
  any scenario's behaviour fails `every_library_scenario_runs_twice_and_after_a_reset_with_the_same_trace`
  and `sandbox-cli scenarios run --all --check` on any developer's machine; removing or
  editing a scenario is a breaking change. That test runs every scenario twice in one
  project (two run ids), resets the dataset, runs each again under the first run's keys
  (new epoch, new identities), requires all three traces equal to the committed one, and
  requires the development store, consuming the signed webhooks, to fulfill exactly the
  payments that captured money, once each, and reconciliation with the network to be
  clean. Reproducibility relies on the reconciler's grace exceeding the network deadline
  (its default, D-036): the first harness set the grace to 1 ms, the reconciler resolved a
  lost reply while its own request still waited, and that request answered `201` instead
  of `202 uncertain`; the default makes the uncertain answer the same on every run.

- **D-070 — The TypeScript SDK: server-side, minor units, honest results, webhook
  verification, tested against the real server.** `packages/sdk-ts` (`@sandbox/sdk`) is for
  a test store's **server** (Node 22.17+, `fetch` and `node:crypto`; no browser build: a
  sandbox key never belongs in a page). It follows D-068 to the letter (the same
  configuration rules, plus `NODE_ENV=production` refused unless
  `allowProductionEnvironment: true` says this is a test of a production build; the same
  retry rule and bounds; `sdk_` + 32 letters `a`–`p` keys) and returns a `MoneyResult` whose
  `settled` is false while the operation is `pending` or `uncertain`, so an uncertain answer
  cannot be mistaken for a paid one; `waitForSettlement` polls until nothing is open or its
  deadline passes and then says `settled: false`, not "failed". Amounts are minor units, as
  in the API (the CLI takes the currency's units, for people). Errors are classes with a
  stable `code`: `ConfigError`, `RefusedError` (status, code, request id),
  `UnknownOutcomeError` (the key to retry with), `UnreachableError`, `NotASandboxError`,
  `WebhookVerificationError`. `verifyWebhook` checks D-020's signature (HMAC-SHA256 over
  `{event id}.{t}.{raw body}`, any of up to four `v1` values, constant-time) and the 300 s
  window before parsing, then that the body names the header's event and says
  `simulated: true`; `fixtures/webhook-vectors.json` holds vectors the Rust signer must
  produce exactly (`the_shared_webhook_vectors_are_what_the_server_signs`) and the SDK must
  verify, so the two cannot drift apart. The package builds to `dist/` with declarations
  (`npm run build`); it is not published (private, like the rest). Tests: `test/unit` (16:
  configuration, retries against a fake fetch, honest outcomes, the vectors) in
  `npm test`; `test/live` (5) run by `scripts/e2e.sh` against the real server, network and
  store: a purchase, a decline, a lost reply settled by the reconciler, a lost **response**
  retried through a proxy that drops it (one payment, `replayed: true`), a conflict, real
  signed deliveries verified (and a tampered one refused), a typed reset and an archived
  payment. The developer guide (`docs/DEVELOPERS.md`) shows `examples/cli-quickstart.sh` and
  `examples/sdk-purchase.mjs` verbatim (`scripts/check-doc-examples.mjs` fails on any
  difference), and `scripts/e2e.sh` runs both, as written, on every check and in CI.

- **D-071 — The production path: one shared host, systemd, loopback behind a TLS proxy.**
  Maintainer request, 2026-11-17: the maintainer will deploy and use the sandbox, so M7 adds
  a tested production path and an operator guide (`docs/OPERATING.md`: install, first run,
  upgrade, backup and restore, troubleshooting). It is the smallest one D-023 allows: one
  Linux host for a team, PostgreSQL, `sandbox-sim` and `sandbox-server` as systemd services
  (`deploy/systemd/`: hardened units, `Restart=on-failure`, a one-shot
  `payment-sandbox-migrate` unit that migrates as the owner, since the server does not own
  the schema, D-038), configuration only through environment files rendered from
  `deploy/env/*.env.example` (every setting the server, the network and the migrate unit
  read, secrets left empty so a forgotten one stops the process with its name), and a
  reverse proxy (`deploy/Caddyfile`) giving the API, the checkout and the dashboard their
  HTTPS names. Every listener stays on loopback; webhooks still reach only loopback
  receivers on listed ports (D-023, D-059), so a store elsewhere reads `GET /v1/events` or
  runs its receiver on the host; clients elsewhere opt in with `allowNonLocal` /
  `--allow-non-local` (D-068). Two changes make this safe. *Public origins:* in the `shared`
  profile only, `PS_CHECKOUT_ORIGIN` and `PS_DASHBOARD_ORIGIN` may be `https://` DNS names
  (written as browsers send them, no `:443`; IP literals and `localhost` refused), because
  browsers reach the sites through the proxy; the dashboard's own-host rule (D-065), its
  `__Host-` cookie (Secure, so HTTPS only) and the Origin checks work unchanged with them;
  `dev` and `ci` keep loopback origins only. *Role passwords:* `sandbox-server setup` used to
  reset `ps_owner`'s and `ps_app`'s passwords to the public development values on every run,
  so re-running it on a deployed host (a restore needs it) reopened the database to the
  development credentials; now a role is created with `PS_OWNER_PASSWORD` /
  `PS_APP_PASSWORD` when given (16–128 of letters, digits and `._~-`) or the development
  value otherwise, and an existing role's password changes only when one is given. Tests:
  `config::tests::the_sites_may_have_public_https_origins_only_in_the_shared_profile`,
  `checkout::tests::public_origins_are_https_names_written_as_browsers_send_them`,
  `roles::tests` (2); `scripts/release-smoke.sh` (release builds installed from the
  templates, the `shared` profile with public origins, the control planes absent, the
  dashboard answering only its own name, checkout URLs on the public origin, the CLI
  quickstart as written, no secret in a log) locally and in CI's `release` job, which then
  installs the units as root and starts, uses and SIGKILLs the server under systemd
  (`scripts/release-systemd.sh`, `systemd-analyze verify` first). Not tested: the Caddyfile
  against a live certificate authority (the proxy is the operator's), and a hosted mode for
  strangers, which stays an open question.

- **D-072 — Migration and restore tests per release: each release leaves its data behind.**
  END_GOAL asks for "migration/restore tests per release"; D-030 gave migrations restore
  tests from M3 on, but only of the current schema. Now each release commits a fixture,
  `fixtures/releases/<version>.json`: every row of a database that release's code wrote
  (two projects, purchases, a decline, a lost reply resolved by readback, a partial capture
  and void, refunds, webhook events and deliveries, a checkout session, a dataset reset and
  a purchase in the new epoch), its last migration and every balance per project and
  epoch. `tests/releases.rs` restores each fixture into a database migrated only as far as
  its release (`MIGRATOR.run_to`), through the journal's own triggers (none switched off),
  applies this release's remaining migrations, and requires: the server's role and
  protections check passes, every project and epoch reconciles with the same balances, a
  request repeated under an old idempotency key replays its stored answer, and new money
  moves. The first fixture is 1.0.0's (migrations 1–13; written as 0.1.0's until D-087);
  the next release adds its own and
  keeps every earlier one, so a migration that cannot apply to an earlier release's data
  fails before it ships. The network's store is not part of a fixture (its format is
  redb's; the drills cover its restarts and restores).

- **D-073 — Sandbox labels end to end, and M7's load scenario.** Every binary says what it
  is: `sandbox-server`, `sandbox-sim` and `sandbox-cli` answer `--version` with the notice
  (the CLI also prints it first on standard error on every run, and the services log it at
  startup); every API answer carries `Sandbox-Simulated: true` and every body
  `simulated: true`, which both clients require; the SDK exports the same notice and its
  live test compares it with the server's; the env templates, units and README of the
  release say SIMULATED MONEY ONLY. Tests: the binaries' `--version` tests, the release
  smoke. M7's load scenario is `perf/scenarios/m7-developer.js` (written and parsed with
  `k6 inspect`, not run: other builders share the machine): test stores as the clients
  drive them (an authorization, its replayed retry, a conflicting reuse of its key, a
  capture, a partial refund, a refund past the capacity, a read; a tenth declined; a fifth
  on a project whose dataset is reset every 30 s under that traffic), about 1,008,000
  requests in the full run, thresholds per step, and a gate of clean reconciliation with
  nothing open; its smoke and full runs are owed.

## v1 readiness fixes (2026-11-17)

The independent v1 readiness review of `895a421` answered "fix first"; each entry below
closes one of its findings, with the test that failed before the fix.

- **D-074 — `sandbox-cli` speaks HTTPS.** D-071 put a shared sandbox behind a TLS proxy and
  the operator guide checks it with the CLI over `https://`, but the CLI was built without a
  TLS stack: against an `https://` API it sent nothing and said the server was not running.
  The CLI now builds `reqwest` with rustls (its default provider, aws-lc-rs; no OpenSSL,
  which `deny.toml` still bans) and the platform's certificate verifier, so the machine's
  own authorities are trusted; `--ca-cert <file.pem>` (`PS_CA_CERT`) adds a private one (a
  proxy with Caddy's `tls internal`, a company CA). A failed handshake is its own error
  (`ClientError::Tls`, exit 5) in plain words: an untrusted certificate names `--ca-cert`,
  a wrong name or an expired certificate says so, `https://` to a plain port says to use
  `http://`, and `http://` to a TLS port says to use `https://`. Nothing was sent, so it is
  never retried and never reported as an unknown outcome. Only the CLI asks for TLS; the
  server and the network still talk only to loopback over plain HTTP (D-023), although a
  workspace build unifies the feature into their `reqwest` too. Tests:
  `tests/tls.rs::the_cli_speaks_https_and_says_plainly_when_tls_fails` (a TLS-terminating
  proxy with a test CA from `rcgen` in front of a real sandbox; it failed before the fix with
  "did not answer (connect …); check that sandbox-server is running"),
  `config::tests::a_ca_file_must_hold_a_certificate`; and the release smoke's new step 6:
  the release API behind Caddy (pinned, checksummed, `scripts/install-caddy.sh`) with its
  local authority, `status` and a payment over `https`, and the untrusted case refused.

- **D-075 — A server error or a gateway's answer on a money-moving call is "no answer".**
  The clients used to end a keyed call at once on any answer without
  `Sandbox-Simulated` ("not a sandbox") and treat a `500` as a refusal ("fix the request").
  Behind the documented Caddy, a server restart during a payment produces exactly those: a
  `502`/`504` from the proxy, or a `500` after the operation committed (D-015 records the
  operation before the network call). A store following the old answer would start over
  with a new key: a second charge. Now, on a call with an idempotency key, any `5xx` from
  the sandbox and any `5xx` without the sandbox header (a gateway speaking for it) are no
  answer: retried under the same key within the attempt and deadline budget (the server
  replays or finishes the recorded operation), then `ClientError::Unknown` (CLI exit 4) /
  `UnknownOutcomeError` (`settled: false`) naming the key. A read through a gateway's
  `502`/`503`/`504` is retried too and then reported unreachable, naming the status; a
  plain `POST` (no key) is not re-sent after a gateway answer. `2xx` and `4xx` without the
  header stay "not a sandbox". Tests (each failed before the fix: the `500` came back as
  "internal error (internal, HTTP 500)", the proxy's `502` as `NotASandbox`):
  `tests/cli.rs::server_errors_and_gateway_answers_on_money_moving_calls_are_unknown_not_refused`
  (500, 502, 503, 504; the binary's exit 4 naming the key),
  `tests/cli.rs::a_server_restart_behind_a_proxy_mid_payment_charges_once` (a proxy stub in
  front of a real sandbox answers `502` after the server applied the payment, `502` again
  while it "restarts", then forwards: 3 attempts, `replayed: true`, one payment), and the
  SDK's `client.test.ts` (the same four answers, the read through a `502`).

- **D-076 — A payment is read as one snapshot.** `GET /v1/payments/{id}` read the payment
  and its operations with two statements on the pool, so a reconciler's `apply` committing
  between them gave "operation succeeded, payment authorizing, authorized 0", which
  `sandbox-cli payments wait` and `waitForSettlement` then reported as settled with stale
  totals (the review saw it in 4 of 6 polling runs). Now `sandbox_db::payments::snapshot`
  reads the payment, its operations and, when asked, its journal entries in one
  `REPEATABLE READ, READ ONLY` transaction (two more round trips per read, on loopback).
  `GET /v1/payments/{id}` and the checkout's view of an attempt use it, and
  `GET /v1/payments/{id}/journal` now answers the payment and its operations beside the
  entries from the same snapshot, which the dashboard's payment page uses instead of two
  requests (the field additions are backward compatible). Test:
  `tests/payments.rs::a_payment_and_its_operations_are_read_as_one_snapshot` holds
  `operations`' table lock in a writer, waits until the reader waits on it, commits the
  operation's and the payment's resolution, and requires all-before or all-after; before
  the fix it read `("authorizing", 0, "succeeded")`.

- **D-077 — The README gets a newcomer to a key; project creation names its credential.**
  README's "Run it" started the server without `PS_BOOTSTRAP_TOKEN` and never said how to
  create a project, and without the token `POST /v1/projects` answered `401 "missing or
  invalid sandbox key"`, which names the wrong credential. "Run it" now sets the bootstrap
  token and the secret key, starts the network and the server once (with both web apps),
  creates a project, makes a first payment with the CLI and signs in to the dashboard;
  it was walked as written (ports moved into the reviewer's range). Project creation
  without the token answers its own code, `bootstrap_token_required` (`401`), saying to
  send the server's `PS_BOOTSTRAP_TOKEN` as a bearer token, or, when the server has none,
  that it was started without one and creates no projects until restarted with it. Tests:
  `tests/keys.rs::creating_a_project_needs_the_operator_bootstrap_token` and
  `without_a_configured_bootstrap_token_nobody_can_create_projects` (both failed before:
  `unauthorized`, "missing or invalid sandbox key").

- **D-078 — The clients' retry bounds, tightened.** Two gaps in D-068's retries. *The
  deadline:* each attempt had its full 10 s timeout whatever was left of the request's
  deadline, so `--timeout 5` could wait 10 s; now an attempt gets `min(attempt timeout,
  deadline − elapsed)` in the CLI and the SDK (tests:
  `tests/cli.rs::retries_never_overrun_the_overall_deadline`, 10.0 s before the fix for a
  1 s deadline; the SDK's "never overruns the overall deadline", which timed out before).
  *Plain POSTs:* a `POST` without an idempotency key (a reset, a key) was re-sent after any
  `429`/`503` try-again answer, but a `503 deadline_exceeded` or `unavailable` can follow a
  commit (a reset counts the archived payments after committing). Now it is re-sent only
  after a refused connection, `429` or `503 overloaded` (the load shedder answers before any
  handler runs); any other failure after sending is "unreachable: it may or may not have
  been applied", which `sandbox-cli reset` turns into "run `sandbox-cli status`: epoch N+1
  means it did" (exit 4) and the SDK documents on `resetDataset`. The attempt's failure is
  now a typed `NoAnswer` instead of a string prefix (tests:
  `tests/cli.rs::a_plain_post_is_never_re_sent_after_an_answer_that_may_follow_a_commit`
  and the SDK's "never re-sends a reset after an answer that may follow a commit"; both
  re-sent before the fix).

- **D-079 — Off this machine, the key travels over HTTPS.** D-068's `--allow-non-local` /
  `allowNonLocal` admitted `http://` to any host, so a sandbox key could cross a network in
  clear text. Now a host other than loopback must also be `https://` (D-074 gave the CLI
  TLS), unless a second, explicit opt-in says the network is private: `--allow-plain-http`
  (`PS_ALLOW_PLAIN_HTTP=1`) / `allowPlainHttp: true`, for a CI service container. The
  refusal names both fixes. Loopback keeps plain `http://`, as the server's own origins do
  (D-071: public origins are `https://` names). Tests: `config::tests` (`http://sandbox`
  refused with only `--allow-non-local`, admitted with both), `tests/cli.rs` (the binary
  exits 2 naming `https://` and `--allow-plain-http`; it tried to connect, exit 5, before),
  and the SDK's `config.test.ts` (`plain_http`, which it accepted before).

- **D-080 — The development store answers `--version` and `--help`.** It ignored its
  arguments, so `sandbox-dev-store --version` started a store on 56082 and wrote
  `sandbox-dev-store.redb` in the current directory. It now handles them like
  `sandbox-sim` (the notice, the settings it reads, an unknown argument refused) before
  anything starts. Test: `fixtures/dev-store/tests/labels.rs` (it was still running after
  5 s before the fix).

- **D-081 — On the host, no service user can read an env file.** D-071 installed the three
  env files `0640 root:payment-sandbox`, and the server and the migrations both ran as
  `payment-sandbox`, so a compromised server process could read `migrate.env`, take the
  schema owner's password and switch off the journal's triggers, undoing D-038/D-058's role
  split on the host. systemd reads `EnvironmentFile=` as root before it drops privileges, so
  the services never need to read them: the files are now `0600 root:root` in a `0700`
  directory, and the migrate unit runs as its own throwaway user (`DynamicUser=yes`). The
  release job asserts the modes, that `payment-sandbox` cannot read any of the files, and
  the migrate unit's dynamic user (`scripts/release-systemd.sh`); the same check against the
  old install line reports `640`.

- **D-082 — The install is tested against PostgreSQL as Ubuntu's packages ship it.** The
  operator guide's `setup` used `postgres://postgres@127.0.0.1:5432/postgres`, but the
  PostgreSQL project's Debian/Ubuntu packages give `postgres` no password and require
  `scram-sha-256` on 127.0.0.1, so step 1 could not work on a stock host; CI never saw it,
  because its PostgreSQL is a container with a password. The guide now runs `setup` as the
  OS user `postgres` over the Unix socket (`sudo -u postgres env
  PS_ADMIN_DATABASE_URL='postgres:///postgres?host=/var/run/postgresql&user=postgres' ...`, peer
  authentication; `setup` needs no file access), and keeps the password form for other
  hosts. The release job now installs `postgresql-18` from the project's apt repository
  (`scripts/ci-postgresql.sh`, which also checks the stock `pg_hba.conf` rules) and runs the
  whole systemd install, its first run, the restart and the backup and restore against it:
  `setup` over the socket, the services over 127.0.0.1 with the operator's passwords. The
  job also asserts that the old passwordless TCP form fails there (the finding's
  evidence, run). Its first run also showed that the socket URL needs `user=postgres`:
  without it the client presented the user name `anonymous` and peer authentication
  refused it, so the guide's URL names the user.

- **D-083 — Backups run by themselves: a timer, a live dump, the network's store.** The
  operator guide gave one-off commands that stopped the server, no schedule, no retention
  and no step to restore `sim.redb` (`deploy/systemd` had only the three services). Now
  `payment-sandbox-backup.timer` runs `payment-sandbox-backup.service` nightly
  (`Persistent=true`): a live `pg_dump --format=custom` as the schema owner (one consistent
  snapshot; the server keeps serving; the dump is read back with `pg_restore --list`), the
  network's store copied while the network is stopped for the copy (payments sent meanwhile
  come back `uncertain` and are settled, D-015), and `/etc/payment-sandbox` (the secret key
  belongs with the dump), into a root-only directory keeping the newest 14 of each; copying
  it off the host stays the operator's job, and the guide says so. The restore puts the
  dump into a fresh database (`setup` keeps the passwords) and the store back, then
  migrates and starts. The release job runs it once on every push against the stock
  PostgreSQL of D-082: a payment, the timer scheduled, the backup (modes, contents, the
  network running again), the restore, the payment found, `reconcile --network` clean and
  a new payment. `scripts/restore-rehearsal.sh` stays the fingerprint-level check of the
  database part (it needs a quiet source); the guide no longer asks to stop the server for
  a backup.

- **D-084 — The checkout and webhook endpoints in the SDK, the CLI and the docs; an API
  reference tested against the routes.** A store learned `capture_method`, `return_url`,
  `embed_origins`, `expires_in` and `client_secret` only from D-060 or the source, and
  neither client could create a checkout session or a webhook endpoint. Now the SDK has
  `checkoutSessions.create/get/waitForPayment` and `webhookEndpoints.create/list/rotate/
  disable`, and the CLI `checkout create|get|wait` and `webhooks create|list|rotate|disable`
  (`checkout get`/`wait` exit 3 for a decline and 4 for "not known yet", like payments).
  Session creation is not keyed, so the clients never re-send it after a failure that may
  have reached the server (D-078); a lost answer means "create another". `waitForPayment`
  ends on `paid` (or `authorized` with manual capture), on a session that can no longer be
  paid (expired, attempts used, conflict) or at its deadline, and says which (`finished`,
  `paid`). `docs/DEVELOPERS.md` §4 shows `examples/checkout.mjs`, which `scripts/e2e.sh`
  runs as written with a stand-in payer (`e2e/payer.mjs`, the page's own request) until the
  session is paid; the CLI quickstart creates and shows a session. `docs/API.md` is the API
  reference: hand-written, and `tests/api_reference.rs` requires its endpoint table to equal
  the routes in `routes.rs` (both ways; a mutation adding a row and removing one fails it),
  every row to be served (never `route_not_found`/`405`), and every stable error code to be
  listed. Tests: `tests/cli.rs::checkout_sessions_from_the_command_line` and `webhook_endpoints_from_the_command_line`
  (before: `checkout` was an unknown subcommand, exit 2), the SDK's unit tests and its live
  test (before: no `checkoutSessions`), the live webhook test now through
  `webhookEndpoints`.

- **D-085 — Every table scroll region is labelled and focusable; the ledger keeps its
  balance on a phone.** At 360 px axe found `scrollable-region-focusable` (serious) on the
  ledger, the webhooks page and a payment's page: their tables scroll sideways in a
  container a keyboard could not reach, and the ledger's Balance column, the page's main
  number, sat off-screen behind that unlabelled scroll. The kit's `Table.Root` now takes a
  required `label`: its container is a `role="region"` with that name, `tabindex="0"` and
  the kit's focus ring, so it is announced and scrolls from the keyboard; the pages' own
  extra `overflow-x-auto` wrappers are gone. The ledger shows Account and Balance at every
  width and folds currency and normal side under the account's name below `sm`. The axe
  journey (`e2e/tests/a11y.spec.ts`) now runs at 360 and 1440 px, light and dark (4
  journeys, every screen of both sites); the phone journey requires the first balance fully
  in view. Before the fix the 360 px runs failed (`scrollable-region-focusable` on the
  payment page among others) and the 1440 px runs passed.

- **D-086 — Dashboard errors in plain words, with a way on; foreign keys never leave the
  page.** The dashboard showed the server's message and nothing else ("payment not found";
  "missing or invalid sandbox key"), sent a pasted `sk_live_…` key to the server, and put
  decision ids in its copy ("(D-028)", "(D-006)"). Now every page that loads data shows a
  failure as the kit's destructive Alert (`LoadError`): a title and plain words for the
  common codes (not found, archived by a reset, no answer, report budget, missing scope,
  busy), the next step (Try again, Back to payments / the ledger; an explanation whose
  cursor a restore invalidated starts over), and the code with the server's message behind
  a Details toggle. Sign-in refuses anything that is not `sk_sim_` and 64 hex characters in
  the page, never sending it, and an unaccepted key says to check the paste or create a new
  one with `POST /v1/keys`. Empty tables say what fills them. The copy carries no decision
  ids. Tests: `e2e/tests/dashboard.spec.ts` (a live-key shape refused with no POST sent; an
  unaccepted key's words; a missing payment's words, its Back link and its code shown only
  after Details; no "(D-NNN)" on the ledger or webhooks pages); the old pages failed each
  of these by construction (raw text, the key sent, no link, ids in the copy).

- **D-087 — The first release is 1.0.0, and its fixture says so.** D-072's first fixture was
  named `0.1.0` while v1 is to be tagged `v1.0.0`, and it sat at migration 13, every
  migration there is, so `tests/releases.rs` applied none to it: per-release coverage
  proves nothing until the next migration exists. The workspace and the SDK are now
  version 1.0.0 (`--version` says so), the fixture is rewritten by that version as
  `fixtures/releases/1.0.0.json` (the writer refuses any other) and `0.1.0.json`, never
  released, is gone. Coverage starts with the first migration after v1.0.0: it must apply
  to 1.0.0's data, reconcile and replay old keys, or it fails before it ships (the review's
  mutation check showed a breaking 14th migration would be caught).

- **D-088 — Web vitals of the production builds, budgeted in the gate.** D-064 budgeted the
  apps' gzip sizes and measured page loads until the payer can act, but not the vitals a
  user feels. `e2e/tests/vitals.spec.ts` measures LCP, CLS and INP in the page
  (PerformanceObserver: LCP before the first input, CLS as the largest session window, INP
  as the longest event entry of a page's scripted interactions, with 16 ms the smallest
  reported) for the checkout and the dashboard's sign-in, overview, payments, a payment,
  ledger and webhooks pages, on the apps' production builds served by the real server, and
  fails on anything over the "good" thresholds (LCP 2.5 s, CLS 0.1, INP 200 ms). It runs in
  every gate through `scripts/e2e.sh`; `scripts/web-perf.sh` records it with the bundle sizes
  and page loads (`perf/results/<stamp>-web-v1.json`). Its first run failed: the webhooks
  page shifted by 0.12 as its tables filled, and every signed-in page by about 0.06 as the
  nav appeared; the pages now show skeletons until their data is in (no "nothing here yet"
  that is not true yet) and the layout holds the nav's height while the session loads,
  bringing every signed-in page to about 0.001.

- **D-089 — Full load runs on GitHub-hosted runners.** The full runs (≥1,000,000 requests)
  were owed since M0 because the laptop is shared: its load decided every smoke. They now
  run on GitHub-hosted runners through a manual workflow (`.github/workflows/load-full.yml`,
  `workflow_dispatch`, one scenario or all): one job per scenario, each on its own runner
  with its own PostgreSQL started from `deploy/compose.yaml` (the local stack's image,
  digest and settings, so lock waits are logged as they are locally), the release build, and
  `MODE=full scripts/load.sh <scenario>` unchanged. Each result is labelled "GitHub-hosted
  runner" with the runner's CPU, cores, memory, OS, image and run id (no host names) and
  uploaded as an artifact, pass or miss; the job fails on a threshold miss after the upload.
  The results are committed under `perf/results/` as they came out, each with a row in
  `perf/README.md`'s load matrix; a miss is kept, never rerun until green. They are
  observations on shared cloud hardware where the load generator, the server, the network
  and PostgreSQL share one 4-vCPU VM, not capacity. `m2-refund-race` runs at its defined
  size (36,000 offered refunds), below the 1,000,000 rule by design: it measures exact
  admission under contention, not volume. Actions are pinned by commit SHA and the token
  is `contents: read`, as in `ci.yml`.

- **D-090 — Every load result says where the CPU went; misses are classified before they
  are explained.** The first GitHub-hosted full runs (D-089) gave five misses but no way to
  tell the runner's limits from the product's: the load generator, the server, the network
  and PostgreSQL share 4 vCPUs. `scripts/load.sh` now records, on Linux, the machine's busy
  share of all cores every 5 s over the run (mean, p50, p90 and the share of samples at
  95% or more) and the CPU seconds of the server, the network and PostgreSQL's container;
  what they do not account for is mostly k6. Each miss is classified: a harness error (the
  run did not measure the product) is fixed and that scenario re-run; a threshold miss is
  kept as it came out, and the CPU record decides whether the runner was saturated (the
  rate is not sized for the machine) or the server spent its time somewhere worth
  profiling; a threshold is never lowered to pass, and a budget set without a measurement
  is said to be one. Two harness errors were found in the first run: `m3-uncertain`'s
  `teardown()` waits up to 150 s per project for the reconciler but had a 180 s limit for
  two projects (it timed out before checking its gate), now 330 s; and every scenario tagged
  requests by their full URL, so k6 kept one metric series per payment (6.4 million in the
  m3 run), costing the generator memory and CPU beside the server; the URL's ids now fold
  into the `name` tag (`/v1/payments/{id}/captures`). The workflow takes a comma-separated
  list of scenarios, so the misses re-run together.

- **D-091 — A pooled connection never carries a transaction to its next user.** The gate
  after D-076 failed once: a journey's `POST /v1/keys` answered 500, "cannot execute
  INSERT in a read-only transaction". sqlx's `begin`, when its future is cancelled after
  `BEGIN` reached the server but before sqlx counted the transaction (a client that
  disconnects mid-request, as a page that navigates away does), leaves the server inside a
  transaction that sqlx's own rollback guard does not see (its depth is still 0), and the
  pool hands the connection on. D-076's `BEGIN ... READ ONLY` made it visible; a plain
  `BEGIN` left behind is worse, since the next user's writes would wait for someone else's
  `COMMIT`. The pool now runs one `ROLLBACK` in `after_release`, off the request's path
  (before sqlx's own release ping), which ends whatever is left and changes nothing
  otherwise; `client_min_messages=error` keeps it from drawing a warning each time. Test:
  `crates/sandbox-db/tests/pool.rs` leaves a `READ ONLY` and a plain transaction on the only
  connection and requires the next user to be outside both (it inherited `transaction_read_only
  = on` before the fix).

- **D-092 — The full runs' misses, classified.** With D-090's CPU record and its two
  harness fixes, the five misses of the first GitHub-hosted run were re-run once and
  classified (both results of each are committed; `perf/README.md` has the table).
  `m6-checkout` was the harness: with the metric series per session gone it passes (p95 24
  ms, p99 104 ms). `m3-uncertain` was the harness first (its gate was never checked), and
  its re-run's gate held (nothing open, journal, payments and network agree over 1.79
  million requests), but 3.35% of its load checks failed with the CPU 64% busy: the product.
  `m2-lifecycle` is the product too: 12% failed and authorize p99 59 s while the CPU was 40%
  busy and almost never saturated, so requests waited rather than computed; the leading
  suspect is the network's durable write per request (one fsync each) under bursts, the
  per-payment queue the other. `m5-explain` is the product: PostgreSQL spent about 2 cores
  summing every journal line per balances call and per explanation page; the fix is derived
  per-account balances kept with each posting and reconciled to the journal, which stays
  the only authority. `m1-auth` is the runner: the machine was saturated in 88% of samples
  with the generator taking about half, and each read cost about 0.5 ms of CPU in the server
  and PostgreSQL together; its 25 ms p95 budget was chosen, not measured, and stays until a
  run with the generator on its own machine. No threshold was changed. The product misses
  are owed fixes, each to be profiled and proven with a new run before v1.0.0 claims them.

- **D-093 — The network reports its own waits.** D-092 could not prove where m2's 59 s
  tails waited because neither side of the link had wait telemetry: k6's
  `http_req_waiting` showed requests unanswered, not why. The simulator now counts its
  write path — batches run, their largest size, the total and the longest durable commit,
  each command's enqueue-to-answer wait, commits that failed and commands shed at the full
  queue — and its listener the accepted, refused, served and active connections, how long
  the over-cap ones waited and the longest wait. `GET /v1/stats` on the network reports
  both under the network credential (control stays off in load runs); `scripts/load.sh`
  writes it into every result as `network_stats`, so the next miss is classified from
  evidence, not from a second round of guessing. `crates/sandbox-sim/src/store.rs` keeps
  the counters next to the writer they measure; `crates/sandbox-http` counts the
  transports it serves.

- **D-094 — A connection waits for a serving place only so long, then is refused.** The
  old accept loop took a semaphore permit before `accept()`: past the connection cap the
  excess waited in the kernel's listen backlog with no deadline and no count, which is the
  shape of m2's tail (authorize median 6 ms, p99 ≈ k6's own 60 s timeout, CPU 40% busy —
  sent and un-answered, not computing). Accepted connections now have an application-level
  wait bounded two ways: a pending bound (`max_pending`, default the connection cap) that
  refuses instantly when full, and a deadline (`accept_wait`, default 2 s) for how long a
  pending connection may wait for a serving place. A refusal answers `503` with
  `Retry-After: 1`, `connection: close` and the sandbox marker inside `write_timeout`, so
  an over-cap client is told quickly and may retry — honest pressure relief instead of an
  unbounded queue the product cannot see. `accept_wait` is deliberately shorter than the
  server's 5 s request timeout, so a request that waited near the deadline is still
  answered inside the budget the caller gave it. Nothing else changed: the cap, the
  read/write timeouts, the per-connection request and age limits, the drain and the
  journal-side waits are as D-054/D-057 left them. Test:
  `crates/sandbox-http/src/lib.rs::a_connection_past_the_cap_is_refused_with_a_bounded_wait`
  holds the only serving place, sends a second connection and requires a 503 well inside
  the client's own patience.

- **D-095 — The network's commit-crash drill.** The server crash drills kill the real
  binary at compiled-in fault points; the network's store never had one, so "answers only
  after its durable commit" was asserted in comments and reasoned, not drilled. Under
  `fault-injection` (never in a release build) the store now has `store_commit`: a batch
  is run and staged inside the write transaction but aborted before `txn.commit()`
  returns — `PS_SIM_FAULT=store_commit`, `PS_SIM_FAULT_AFTER` picking which hit.
  `crates/sandbox-sim/tests/drills.rs` runs the real `sandbox-sim` binary, executes two
  commands to clean answers, then crashes the third batch. Afterwards the acknowledged
  ids are still `executed`, the unacknowledged one reads back `unknown` and never appears
  as executed, and its retry under the same identity executes once — what the crash left
  is exactly what the caller was told, nothing more.

- **D-096 — Balances are derived per account, kept with each posting, reconciled to the
  journal.** `/v1/balances` and every explain page summed the epoch's journal lines on
  each call; the m5-explain full run (D-092's classification) spent its database time on
  those repeated scans. `account_balances` (migration 0014) now holds one row per
  `(project, dataset_epoch, currency, account)` — `balance_minor` and `line_count` — and
  the same statement that inserts an entry's lines adds its per-account deltas
  (`INSERT … ON CONFLICT DO UPDATE`): a committed line is inside its balance by
  construction, no worker or read-repair needed. The journal stays the only authority:
  the table is a derivation the backfill recreates, and reconciliation re-derives every
  balance and line count from the lines and reports drift, missing or phantom rows in a
  new `balance_mismatches` field — `a_drifted_derived_balance_is_reported_not_trusted`
  proves a planted +1 is reported while the journal's own checks stay clean. The explain
  pages keep their snapshot honesty: the horizon balance is the live derived balance
  minus the lines invisible to that snapshot (the usually-small recent tail), computed in
  one statement so the two reads share a moment; the page's lines still come straight
  from the journal. Epoch is part of the key because a posting caught mid-reset can
  legally land in the archived epoch — a balance column on `accounts` (which has no
  epoch) would have silently folded it into the new epoch. A new blocking edge is honest
  about it: two postings to the same account now serialize — the balance row lock is held
  until the posting transaction commits — so an entry held open stalls same-account
  postings. The real posting transaction is short (the network call sits outside it), so
  the held window is milliseconds; the alternative (deferred derivation) would break the
  same-transaction guarantee. Measured before merge (`perf/results/20261119T1240Z-posting-hotrow-bench.json`,
  development observation, M2 Pro, busy machine): one row per account flattened at
  ~1,100 postings/s from 8 concurrent tasks against a journal-insert baseline of
  7,859/s at 64 — the hot-row ceiling the coordinator predicted. Each entry now carries
  one of **16 stripes** (`entry_id` hash, the uuid's random low bits) so concurrent
  postings mostly touch different rows: 5,484/s at 64, ~5x the unstriped number; the
  residual gap to baseline is the upsert's own work plus ~1/16 same-stripe collisions.
  Reads and reconciliation sum the stripes, staying O(accounts x 16). The upsert's row
  locks are taken in the SELECT's `ORDER BY (currency, account_kind, stripe)` — a total
  order shared by every posting, so concurrent postings can never deadlock even if an
  entry ever carries lines in two currencies (today entries are single-currency by
  schema: `journal_entries.currency` is the only currency field and every line takes it).
  Coverage: `ordered_upserts_cannot_deadlock_across_currencies_and_reversals` races
  entries and their reversals across all four currencies. Tests:
  `derived_balances_are_kept_per_epoch`, `concurrent_postings_keep_the_derived_balances_exact`,
  the drift test above, and the existing random-postings property — its `balances()` read
  now goes through the derived table, so it is also the derived-equals-journal property.

- **D-097 — Name the misses: status counts per op, a drain window for the open gate.**
  The m2/m3 full re-run on `1a97f99` (CI `44517004777`, kept:
  `perf/results/20261119T114227Z-m2-lifecycle-full.json` and `…T114231Z-m3-uncertain-full.json`)
  failed ~26%/~20% of requests with authorize p95 = p99 ≈ 2.0 s, and "no operation left
  open" at teardown. The summary could not say *which* answers the failures got — the
  plateau could have been timeouts, sheds or honest `uncertain`s. Three measurements
  closed the classification: (1) the run's own `network_stats` showed the sim's listener
  never saturated (`refused=0`, place waits ~0) and its store commit-bound
  (`commit_ms_total` ≈ `batch_ms_total`, ~2.2 ms/commit, max ~1.5 s on the shared
  runner's disk) — a queue under the 2 s `sim_timeout`, never a shed (`shed=0`); (2) a
  local reproduction at the same arrival rate per core (`RATE=285 ROUNDS=1`, ~830 req/s
  on 12 cores vs 284 req/s on the CI's 4) reproduced the exact 2.0 s pin with the sim
  **idle** (waits ≤64 ms, commits ≤34 ms, 0 sheds) — the server's *own* listener
  saturated instead: `cap=512, pending=512` in the server log, plus `PaymentBusy` 503s
  from the payment-queue wait. Once latency stretches, k6's arrival-rate executor ramps
  VUs (1,025–1,026) — each stalled request holds a connection, the 1,024 places fill,
  and pending connections ride `accept_wait=2s` to a named 503: the plateau is D-094's
  bounded refusal, not a silent timeout, and the instant-refusals show as
  `http_req_duration min=0s`. (3) The leftover ops were `pending`/`uncertain` ops still
  inside `reconcile_grace` (3 s) or their `next_check_at` backoff, plus a sweep draining
  a ~300k backlog at its bounded rate — "not yet closed," never lost; the journal was
  clean in both runs. Changes landed: `op_status{op,status}` counter in `perf/scenarios/lib.js`
  with `statusThresholds()` enumerating plausible codes per op (the JSON summary only
  keeps threshold-referenced sub-metrics); a bounded drain window in the reconcile gate
  (`RECONCILE_DRAIN_MS`, 60 s default — m3 already drains 150 s); `RATE`/`ROUNDS` env
  overrides on m2 for per-core-equivalent reproductions; `load.sh`'s machine telemetry
  probes (`ps`, `sysctl`) now degrade to placeholders under `pipefail`+`set -e` instead
  of aborting the run's result on a restricted dev machine. Still open for the next run:
  whether the ~4-core runner can hold the offered rate at all — the refusal path is
  already honest and now visible, so a capacity miss will report named 503s, not a
  2 s-shaped question mark. Candidate knobs, deliberately not tuned on this evidence:
  `PS_DB_MAX_CONNECTIONS` (32, 1 s acquire), the listener's 512/512, and a batching
  linger in the sim writer for its fsync ceiling.

- **D-098 — The explain horizon bounds its invisible tail by the snapshot's xmin.** The
  m5-explain full run on `d7b9740` (CI `44528148592`, kept:
  `perf/results/20261119T125632Z-m5-explain-full.json`) failed ~11% of balances and ~9% of
  explain reads with a bimodal shape: median 2 ms, p90 2 s — fast reads plus a
  backlog/refusal tail, not slow answers across the board. `EXPLAIN (ANALYZE, BUFFERS)`
  on a fabricated 100k-entry / 200k-line epoch (`tests/explain_plans.rs`, committed:
  `perf/results/20261119T1357Z-explain-plans.json`, development observation) found one
  real query defect: the horizon's invisible-tail scanned **every entry of the epoch**
  per call — a parallel seq scan of 100k rows removing all by filter, 9.36 ms — so read
  cost grew with the journal exactly as feared, and ~575 explain calls/s of that is ~5
  core-seconds of scan per second on a 4-core runner. Entries invisible to a snapshot
  necessarily have `created_xact >= pg_snapshot_xmin(snapshot)` (in-flight then, or
  committed after — a strict superset of the invisible set; restored rows, whose
  `xmin <> created_xact`, are treated as always-visible and unaffected). Migration 0015
  adds `journal_entries_by_xact (project_id, created_xact)` and the horizon query takes
  the bound: **9.36 ms → 0.10 ms**, an index range visiting ~0 rows. The other reads were
  already cheap at scale: the stripe-sum balances read is 0.035 ms, the explain page
  0.34 ms (entries-by-seq + lines-PK probes). Not fixed by this entry: the 2 s tail is
  also the shared API listener's bounded `accept_wait` when stalled VUs fill its places —
  the same admission-backlog shape as m2 (D-097). A 2 s pending wait on a 50 ms-p95 read
  endpoint is a real design smell the coordinator flagged; the honest options (a shorter
  read-side wait or reads on their own listener/admission budget) need their own
  measurement and are deliberately left open rather than tuned here. The scenario now
  records `op_status{balances,explain}` counts so the next run's failures name themselves,
  and `RATE`/`SECONDS`/`SEED` allow per-core reproductions like m2's.

- **D-099 — Reads admit separately from writes, at both layers.** The ~2 s pin the
  classified runs kept showing (D-097, D-098's open tail) is a *shared* admission
  budget: a connection's wait for a serving place (`accept_wait` 2 s, D-094) is FIFO —
  under a write flood, a read connection queues behind write connections it would
  outlive a hundred times over, and past the place the shared `max_in_flight` gate can
  be entirely held by writes. Two layers now separate them. **Requests**: `GET`/`HEAD`
  take their own permits (`LoadGate`: `PS_READ_IN_FLIGHT`, default 128, over
  `PS_MAX_IN_FLIGHT`'s 512) and wait at most `PS_READ_QUEUE_WAIT_MS` (100 ms) — writes
  shed instantly as before; a refused read is the same named `503 overloaded` +
  `Retry-After`. **Connections**: `PS_READ_LISTEN` binds the same router on a second
  listener (the "read edge") whose own serving places and `PS_READ_ACCEPT_WAIT_MS`
  (100 ms) mean a write flood can never queue a read connection ~2 s. `load.sh` binds
  it on `port+4`; m5's reads now go through it (`READ_URL`), so the re-dispatched run
  measures the fixed path. Measured (`perf/results/20261119T1723Z-read-edge-probe.json`,
  development observation, M2 Pro at load 33–150 — a machine the slot's other worktree
  was saturating; the comparison is intra-run, both edges under the same conditions):
  writes flooded the main listener at ~855 offered/s, 45k of 56k authorizes refused —
  all named 503s, ~117 status-0 timeouts in 61k requests. **Refused** reads on main
  showed the old split (min 9–11 µs instant sheds, p99 ≈ 2.01 s place waits); on the
  edge every refused read had a **~1 s floor** — `PS_DB_ACQUIRE_TIMEOUT_MS` — i.e. it
  always reached request handling (conn place ≤100 ms, read permit granted) and was
  refused only at the shared database pool. The 2 s admission plateau is gone on the
  read path; the residual shared budget is the 32-connection pool itself, whose reads
  still lose to held write work — a pool-level reservation is deliberately left out
  (bigger surgery, and under a healthy machine the acquire deadline, not the queue,
  bounds it). Everything still refuses honestly: named 503, `Retry-After: 1`.

- **D-100 — The database pool is bulkheaded like the HTTP layers.** D-099's residual:
  the shared 32-connection `PgPool` let a write flood hold every connection, so reads
  that had won their conn place and read permit still died on the acquire deadline.
  Reads now have their own pool. **Budget**: `PS_DB_MAX_CONNECTIONS` remains the whole
  budget the app may hold (default 32 — far under PostgreSQL's `max_connections`, so
  migrations, backups and admin sessions always connect). **Split**: `PS_DB_READ_CONNECTIONS`
  (default 8) is the read pool; writes keep the remainder (24 default — validation
  requires ≥1 write and ≥1 read). The droplet's Postgres also runs the lazy migrate
  pool (sequential, ~1 conn) and backup, which never share the app's budget.
  **Deadlines**: each pool keeps its own acquire timeout — writes
  `PS_DB_ACQUIRE_TIMEOUT_MS` (1,000 ms), reads `PS_DB_READ_ACQUIRE_TIMEOUT_MS`
  (500 ms): a saturated *read* pool refuses fast too, on its own budget.
  **Classification**: one rule at both layers — `is_read_method` (`GET`/`HEAD`) picks
  the `LoadGate` budget *and* `AppState::db_for` picks the pool, so a GET's auth lookup
  and a dashboard session check pay from the read pool as well. Handlers take their
  pool explicitly: `GET`/`HEAD` handlers and the helpers they call take `read_db`;
  writes, background tasks (reconciler, deliverer, lifecycle) and migrations take
  `write_db`; shared helpers (`payer_view`, `authenticate`, `network_agreement`) take
  a `db` parameter so the caller's classification reaches them. `readyz` pings the read
  pool. **Tests**: `tests/pool_bulkhead.rs` — `reads_answer_while_the_write_pool_is_held`
  holds every write connection and `/readyz` still answers 200 inside 500 ms;
  `a_shared_pool_starves_the_same_read` keeps the pre-bulkhead world as the red half:
  one pool for both duties refuses the identical request after the full acquire
  deadline. Sizing honesty: 8 read connections is ~25% of the budget for reads whose
  measured cost is sub-millisecond at 100k entries (D-098's plans); the conn count is
  never the read path's binding constraint — CPU is — and reads saturated *anyway*
  still get a named 503 on its own deadline, not a write's.
- **D-101 — One classification, enforced, with read-only reads (Colophon's D-225 rule,
  held here so the two products cannot diverge without reason).** D-100 split the
  pools; this aligns the *rule* with Colophon's. **Classify once**: `classify_pool`,
  a `route_layer` beside `shed_load`, applies `is_read_method` (`GET`/`HEAD` → read,
  everything else → write) once per request and inserts `DbPool` into the request
  extensions — `/v1` handlers extract `Extension<DbPool>` and never name a pool, so
  the classification cannot drift per handler. Auth reads the same extension, so a
  read's credential check still pays from the read pool; `db_for` is the fallback
  for the sites' routers (which mount `/v1` handlers behind the same layer).
  **Read-only POSTs opt in, compile-checked**: `middleware::post_read` is the only
  way a `POST` route reaches `read_db` — it marks the request `ReadOnly` and swaps
  the inserted `DbPool`; a plain `post(handler)` can never produce the marker. No
  read-only POST exists today; registering one is now a deliberate, reviewable act.
  Its admission still pays the write-side budget (a POST body is shaped like a
  write); the pool is the resource that matters for starvation.
  **Reads are read-only at the database too**: the read pool's connections carry
  `default_transaction_read_only=on` — every implicit or explicit transaction is
  `BEGIN READ ONLY` in effect — so a read path that tries to write fails loudly
  (SQLSTATE 25006) in the test that introduces it, never silently works.
  **Independent deadlines**: read acquire `PS_DB_READ_ACQUIRE_TIMEOUT_MS` (1,000 ms —
  the D-225 "one second, then 503 + `Retry-After`"; the 503+`Retry-After` mapping was
  already true via `DbError::Unavailable`) and read statement
  `PS_DB_READ_STATEMENT_TIMEOUT_MS` (1,000 ms) are their own knobs.
  **Bookkeeping writes during reads**: none exist (verified at the change — every
  `read_db` call site is a SELECT); any future read path that wants one must use
  `write_db.try_acquire()` and skip on `Err`, never await a saturated write pool.
  **Metrics**: `AppState.pool_stats` — per pool, a supervised `pool_metrics` task
  samples in-use occupancy (histogram, max, mean) and `timed_acquire` records
  acquire-wait and timeout counts at explicit acquisitions; implicit acquires inside
  `execute(&pool)` surface as the sampled occupancy plus the per-op 503s k6 already
  counts (sqlx exposes no acquire-wait hook — that is the honest coverage).
  **Proof**: `tests/pool_bulkhead.rs` keeps the D-100 halves and adds
  `a_write_on_the_read_pool_is_refused_loudly` (SQLSTATE 25006 on the read pool;
  the same statement succeeds on the write pool) and
  `a_post_reaches_the_read_pool_only_through_post_read` (a plain `post` draws the
  write pool; `post_read` draws the read pool — observed by pool size).

- **D-102 — Capacity is measured, not assumed; reads are one round trip (Colophon's
  D-221/D-223/D-227 rule, same problem).** Run `44658104817` proved a full run offered
  above the runner's capacity can only measure shedding — its p99 is the client's
  deadline, not a latency. The rule, in three parts.
  **Measure the knee**: `scripts/load-capacity.sh <scenario>` steps the offered rate
  through short `MODE=full` runs (load.sh passes `RATE`/`SECONDS`/`SEED`/`ROUNDS`/
  `READ_RATE` through to the scenario) until the run's *own thresholds* fail, then
  records the last passing rate — the knee — with every step, the machine record
  (CPU model, cores) and a `recommended_full_rate` at 60% of the knee.
  **Evidence runs at ~60% of the knee**: below the knee the run measures latency and
  errors, which is what a threshold judges; above it the run measures only the shed.
  60% was chosen over 50/70: it is far enough below the measured knee to survive
  machine-class variation between the probe and the CI runner, high enough that a
  real regression still fails loudly. Overload is not deleted — it moves to its own
  label: `m8-overload.js` deliberately offers above capacity and asserts the shed's
  *shape* (every refusal 503 + `Retry-After`, fast, never `status:0`, zero 500s).
  **Cut round trips, the measured lever** (why m5 shed at ~1,200 rps with PostgreSQL
  at 753 of 906 CPU-s): every statement's plan is sub-millisecond at 300k lines
  (`explain_plans.rs`, re-run this change — balances 0.043 ms, horizon 0.058 ms, page
  0.316 ms), so the saturation was per-statement fixed cost, not row work. Explain
  paid five round trips per first page (auth resolve, generation, snapshot, horizon,
  page) and two per subsequent page. Now: the whole explain is ONE statement
  (`reconcile::explain` — generation + as-of + horizon + page via `g`/`h` CTEs and a
  LEFT JOIN LATERAL page), and key resolution goes through a bounded
  `KeyCache` (1,024 entries, oldest-evicted, 30 s TTL backstop) — in-process
  `revoke`/`reset` invalidate exactly, so `a_revoked_key_fails_on_its_very_next_request`
  still holds literally. Result per request: explain 5 RT → 2 (auth miss) / 1 (hit);
  balances 2 → 1. The bulkhead's pools, the read-only rule and the named-refusal
  contract are unchanged — this changes how many trips a request costs, not what it may do.
  **Addendum (the cross-process hole, S2):** `KeyCache` invalidation was exact only
  inside the process that changed a key — a second server on one database (restart
  overlap, a second instance), or a CLI/psql writing `revoked_at` directly, left the
  key alive elsewhere for up to the TTL. Chosen over a single-writer advisory lock
  (option (a), which would refuse a second server's startup and complicate rolling
  restarts): **cross-process invalidation, option (b)**. Migration `0016` adds
  triggers on `api_keys.revoked_at` and `projects.dataset_epoch` that
  `pg_notify('auth_epoch', …)` — the database itself broadcasts the change to ANY
  writer, so a revoke through another server, the CLI (which goes through whichever
  server it calls) or a psql all land identically. Each server runs the supervised
  `auth_epoch_listen` task: one `PgListener` connection held for its life outside the
  pools plus a second for the liveness self-ping (a fixed **+2** over the pool
  budget, deliberate — a deployment sized at +1 fails every ping connect, so the
  ping never lands and the loop keeps clearing the cache), applying `k<pid>:<kid>` →
  `invalidate_key` and `p<pid>` → `invalidate_project` on commit. A lost connection
  may have lost notifications — the cache flushes whole on reconnect, and the 30 s
  TTL shrinks to the missed-notification backstop for the gap before the listener
  attaches. Re-attach is bounded backoff — doubling with jitter — not a fixed
  pause, so a peer that accepts then kills every socket cannot spin the loop,
  and the whole connect pair runs inside a deadline that also watches `stop`
  (a peer that accepts then stalls cannot park the task forever). Red-first proof:
  `a_revocation_in_another_process_reaches_this_cache_on_its_next_request` builds two
  independent AppStates/routers/pools on one database, caches the key in B, revokes
  through A's API, and requires B's next request to answer `401`.

- **D-103 — The idle close and the head bound are different limits.** CI run
  `44737615627` (m2-lifecycle) lost 26% of checks to `readLoopPeekFailLocked`: the
  server closed keep-alive connections at the 10 s `read_timeout` — D-054
  deliberately reused hyper's head timer as the idle timeout — while k6/Go pools
  reuse a connection for 90 s of idleness. Every reuse past 10 s raced a close the
  server had already committed to: a transport error mid-request, on a POST
  (non-replayable), that no threshold should ever have to absorb. `read_timeout`
  conflated two different bounds: *how long a head may take to arrive* (slowloris,
  wants ~10 s) and *how long a keep-alive connection may sit unused* (a pooled
  client's private state, wants to be longer than the longest client pool TTL or
  the server always closes first and always races reuse). They are now separate:
  `read_timeout` (10 s) bounds head arrival **in total** — a trickling head dies at
  the bound, not at the stall between bytes — and pauses inside a request body; the
  new `idle_timeout` (120 s, `PS_IDLE_TIMEOUT_MS`) reaps only a connection with
  nothing in flight and nothing arriving, via `graceful_shutdown` — never
  mid-request, and longer than common pool TTLs so the client closes first.
  `max_requests`/`max_age` still close by advertising `Connection: close` on the
  last response before the cap. Detection lives in a `ConnState` shared between
  the byte watcher, the request counter and the reaper tick; only real bytes count
  as activity (an idle connection still flushes — counting that kept it "busy"
  forever and reaped nothing, caught by the unit test). Red-first:
  `an_idle_connection_is_reaped_at_the_idle_timeout_not_the_read_timeout` reuses a
  connection past the read timeout and requires the request to answer (pre-fix:
  dead socket); `a_client_reusing_across_the_request_cap_reads_every_answer`
  requires every response whole plus `Connection: close` on the capped one;
  trickling-head and never-sent-head cases prove the head bound still fires.

- **D-104 — The re-review remediations (S1-1, S2-1..S2-4).** The independent review of
  the unmerged branch found five blocking defects; each fix is named here with its
  proof.
  **S1-1 (cache resurrection + archived-epoch writes):** `KeyCache` gains a generation
  counter bumped under the map lock by every `invalidate_key`/`invalidate_project`/
  `clear`; a miss reads it before `resolve` and `put_if_unchanged` refuses the insert
  when an invalidation landed mid-flight — a resolve can never resurrect a revoked key
  for the TTL. And because a lagging identity's epoch is only a *hint* until verified,
  `begin_authorize`/`begin_child` take `SELECT … FOR KEY SHARE` on the project's epoch
  inside the write transaction (`ensure_epoch`): reset's `FOR UPDATE` conflicts with
  it, so a write either precedes the epoch move or fails `EpochMoved` → 409 — it never
  lands in the archived epoch. In-flight operations still resolve in their own epoch
  (D-022): the guard is on beginnings only. Proofs:
  `a_resolution_older_than_an_invalidation_is_never_cached` (unit),
  `a_write_scoped_to_an_archived_epoch_is_refused` (journal.rs).
  **S2-1 (dropped revocations on reconnect):** sqlx's `recv()` reconnects silently and
  drops the gap's notifications, `split_at(1)` panicked on empty/multibyte payloads,
  and the restart-after-panic never flushed. The loop now drives `try_recv()` —
  `Ok(None)` or `Err` is a gap: flush and re-attach — clears after `listen()` lands
  (covering the whole un-listened window), clears once at startup, and parses with
  `strip_prefix`, falling back to `clear()` on anything malformed (any role may
  NOTIFY). Proofs: `a_revocation_committed_in_a_listener_gap_still_flushes` kills the
  LISTEN backend (`pg_terminate_backend`), revokes in the gap, and requires 401 after
  a NEW backend attaches — the attach is waited on via `pg_stat_activity`, no guessed
  sleep; `a_malformed_notification_flushes_the_cache_without_panicking` covers the
  payload forms.
  **S2-2 (refusal RST):** the over-pending refusal wrote a raw 503 and dropped the
  socket — Linux answers close-with-unread-data with RST, and k6/Go counted ~1,620
  `status:0` transport errors (run `44835402763`) where the contract says every answer
  is a named status. The refusal is now a lingering close: response bytes,
  `shutdown(write)` (FIN after the bytes), then a ≤1 s drain of what the client was
  still sending so the final drop is a FIN. Proof: the rewritten
  `a_connection_past_the_pending_bound_is_refused_at_once` also asserts the whole
  response arrives and `read_to_end` completes — a RST would surface as a read error,
  not bytes.
  **S2-3 (header memory):** hyper's ~408 KiB per-connection head buffer became
  `max_buf_size(32 * 1024)` (~16 MB at the cap instead of ~209 MB); the header-count
  bound stays httparse's 100. Proof: `a_head_past_the_buffer_bound_is_refused` — 16 KiB
  → 200, 64 KiB → 431.
  **S2-4 (descriptor budget):** a startup check sums `cap + pending` over every
  listener (API, read edge, each site) plus pools, the listener, clients and a 320-fd
  reserve, against `RLIMIT_NOFILE`: a low soft limit is raised to the need up to the
  hard bound; beyond it, boot refuses with the budget spelled out. The sites' caps
  dropped to 128 connections (`SITE_MAX_CONNECTIONS`) — static files get a share of
  the budget, not the API's 512+512. Worst case now fits `LimitNOFILE=4096` with room.

## D-105: Sealed, leased cursors; named body and pool deadlines (S3 remediation)

  The second review's S3 list, remediated; every fix has a red-first proof.
  **Cursors are ciphertext, not signed plaintext (S3-1):** the old format encoded
  `context|generation|as_of|after_seq|running` in the clear under an HMAC tag — the
  generation names the cluster's system identifier and the snapshot its global
  transaction ids, a write-rate side channel every tenant's cursor handed to every
  other tenant. Now the wire form is `letters(nonce ‖ ChaCha20-Poly1305(payload))`,
  the same AEAD `secrets.rs` uses: the explain context (project, epoch, currency,
  account — D-056's binding) is the AEAD's associated data, so a cursor of another
  context does not decrypt at all, and nothing inside is readable. The explain
  response's own `generation`/`as_of` fields get the same treatment as opaque keyed
  labels (`CursorKey::label`, HMAC/128, letters-only): a tenant can still tell a
  restore happened — the label changes — but cannot read the identifier out of it.
  A cursor is also a lease, not a bearer token: the payload carries `issued_at` and
  `open` refuses one older than its `PS_CURSOR_TTL_MS` lease (default one hour)
  with `410 cursor_expired`; a clock that cannot be read or sits behind the issue
  time fails closed. A cursor under a different key is now `400 invalid_cursor`
  rather than the old unverified-plaintext path: the `stale_cursor` answer survives
  exactly where it matters — same configured key, old generation, caught by the
  statement's own check (D-050). Proof: `the_sealed_form_reveals_nothing`,
  `expired_cursors_are_refused_by_name` (unit), `forged_explain_cursors_are_refused_cleanly`
  and `expired_explain_cursors_are_refused_by_name` (wire), and two mutants —
  dropped context binding and a skipped lease check — both die.
  **No constant cursor key (S3-2):** `getrandom` failure used to fall back to a
  fixed key, which made every cursor forgeable to anyone who read the source. There
  is no fallback: no `PS_SECRET_KEY` and no OS randomness means the process refuses
  to start (`CursorKey::with_randomness` makes the refusal testable; `new` panics
  on it as a documented invariant). Proof: `without_os_randomness_there_is_no_key`.
  **Read-pool deadlines validated (S3-3):** `problems()` checked only the write
  pool's statement timeout against the request deadline, so a read-pool acquire or
  statement deadline could outlive the request's own deadline and never refuse
  first. Now both read-pool deadlines and the write pool's acquire deadline are
  checked the same way (`PS_DB_READ_ACQUIRE_TIMEOUT_MS`,
  `PS_DB_READ_STATEMENT_TIMEOUT_MS`, `PS_DB_ACQUIRE_TIMEOUT_MS`).
  Proof: `every_pool_deadline_must_beat_the_request_deadline` (red first).
  **A timed-out body is not "too large" (S3-4):** every `Bytes` rejection mapped to
  `413`, so a dribbled body and an oversized one answered alike. The rejection's
  error chain is now walked for the body deadline's `TimeoutError`/`Elapsed`: a
  deadline answers `408 body_timeout`, anything else stays `413`.
  Proof: `oversized_and_slow_bodies_are_cut_off` asserts the named code on the wire.

## D-106: Transport and listener remediation — the re-review's N-register

  The re-review found `9dfe7ef`'s transport work still wrong and the listener/cache
  layer carrying follow-on defects; every fix has a named proof.
  **No implicit header timeout (N1):** hyper 1.12 applies its default 30 s header
  timeout whenever a timer is set — and re-arms it on every keep-alive idle wait,
  so every pooled connection died mid-idle instead of at `idle_timeout`. We pass
  `.header_read_timeout(None)`: the reaper already bounds a slow head on total time
  (the stronger bound — a byte cannot reset it) and reaps genuine idle at
  `idle_timeout`; hyper's default is only duplicated, weaker logic.
  **Refusals read before they answer (S2-2 redo):** the refusal now drains the
  request head (≤1 s, ≤8 KiB) before writing its 503 — answering without reading
  left the client's bytes unaccounted for at close, which is what turns a FIN into
  a RST. **Drains are a bounded resource (N4):** `REFUSAL_PERMITS` (64) bounds
  sockets mid-drain; over it the refusal closes at once — bounded descriptors
  outrank an answer to a refused connection — and the count sits in
  `descriptor_need`.
  **The listener proves its own liveness (N3):** `eager_reconnect(false)` so a lost
  connection surfaces instead of being re-attached inside sqlx with the gap's
  notifications dropped; each beat the loop `pg_notify`s itself through the write
  pool — a delivered notification is a live wire, one whole silent beat is a dead
  one (half-open sockets report nothing else) and the connection is rebuilt with
  the cache flushed. The beat lands in every select branch (N5): notification load
  can starve the timer, never the heartbeat.
  **Cache invalidation is project-scoped (N6):** the global generation is replaced
  by per-project invalidation windows — a resolve compares its start time against
  its own project's marker (and the global one for clears) — so one tenant's
  revokes stop cancelling every tenant's in-flight resolves; malformed-notification
  clears are bounded to ~1/s with a suppressed counter, since any role can NOTIFY
  garbage. Test hooks carry unique `application_name`s so `pg_terminate_backend`
  can only ever hit the listener it names (N2).
  **Mutation proof means red, not nonzero (N7):** a kill now requires
  `test result: FAILED` — a mutant that cannot compile is `INVALID` evidence, not
  a dead mutant; mutants targeting one test carry `__tag` suffixes; and
  `checkout::create` runs under `ensure_epoch` like every other scoped write.

## Open questions (still open)

- **Billing scenario order and definitions** — the default order is
  settlement → fees → payouts → disputes → invoices → recurring, because
  each later scenario reads settled balances; each gets its own contract
  when reached.
- **Hosted mode** — quotas per project, the private-range egress guard and
  sign-up abuse limits, if a hosted instance is ever offered. (D-071's shared host is not
  one: a team's own host, behind its proxy, with loopback-only egress.)
