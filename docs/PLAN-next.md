# Next slice — M8.1: authorization expiry on virtual time, over the server-side inbox

ROADMAP's only unstarted milestone is M8 ("billing direction … each with its own
contract", D-031). Its listed build order puts the **server-side inbox** first —
D-031 already schedules it: every asynchronous simulator outcome (authorization
expiry, settlement, disputes) needs it — and **authorization expiry on virtual
time** second. M8.1 ships both together: an inbox with no consumer is plumbing,
and expiry is the smallest asynchronous outcome with a real user outcome.

## Scope

- `POST /v1/payments` accepts `expires_in` (virtual seconds, bounded) on the
  authorize; the payment records the virtual `expires_at`.
- The simulator holds the authorization record; when its virtual clock passes
  `expires_at` (tick or clock jump), it emits an `authorization_expired` outcome
  into the server's inbox (a new sim → server channel, credentialled like the
  control plane, loopback-only, bounded bodies, at-least-once).
- The server's inbox consumer applies each event **once** by its delivery id,
  inside the same identity discipline as readback: it posts the release (void)
  entries, frees the reserved capacity, closes the operation with a named
  `authorization_expired` outcome, and commits an outbox event in the payment's
  M4 sequence — all in one transaction.
- A capture attempted at or after expiry is refused by name; a capture in
  flight when the expiry arrives wins or loses by the identity's fence, never
  both: capacity is released exactly once.
- Reconciliation counts an open-but-expired hold honestly (not drift) and a
  lost inbox event surfaces as an `uncertain`-class item for the reconciler,
  never silent.

## Failure cases → drills; attack classes → mutation proof

| Failure case / attack | Proof |
|---|---|
| Expiry races an in-flight capture; capture at/after boundary | Test: exactly one wins, capacity released once, second writer refused by name (extends the D-027 race suite) |
| Virtual clock jumps backward after expiry posted | Posted history byte-identical; the expiry does not re-fire (drill-b precedent, simulator half) |
| Crash between inbox row committed and outcome applied | Replay applies the event once — no double release, no duplicate journal rows (drill-a shape) |
| Inbox event for an archived epoch | Fenced by the epoch guard, no write (precedent: `checkout::create`) |
| Duplicate / reordered inbox deliveries | Dedup by delivery id; a stale `expired` arriving after `capture_succeeded` is fenced, not applied over it |
| Forged / malformed inbox payload (any caller can POST) | Refused, never applied; malformed bodies rate-limited like the notify listener |
| **Mutants into the proof** | remove inbox dedup → double-apply caught; remove expiry-vs-capture fence → capture of expired hold caught; skip epoch guard on apply → archived-epoch write caught; drop the outbox event on auto-void → delivery-sequence gap caught; expiry boundary off-by-one (`<` vs `<=`) caught |

## Load scenario — `perf/scenarios/m8-auth-expiry.js` (1M-scale)

Authorize at ~100 req/s with mixed `expires_in`, ~15% expiring under a
mid-run virtual-clock jump, captures racing the expiries on a hot account.
Thresholds: expiry applied within a bounded lag of the jump; zero
double-release; every over-boundary capture refused by name; journal balanced;
`?network=true` reconciliation clean; inbox depth bounded; p95/p99 budgets per
the m2 pattern. Smoke locally (busy-machine observation), full run on CI.

## Risks and open decisions (record as D-NNN when taken)

1. **Lost-delivery backstop**: the inbox is at-least-once; if the sim dies
   between the event and its delivery, does the server also sweep open holds
   past `expires_at`, or does the reconciler's `uncertain` surface suffice?
   A sweeper is a second writer of the same release — the fencing must be
   designed once, for both.
2. **Event ordering**: inbox events and API writes hit one identity's fence —
   the ordering discipline (per-identity sequence on the sim side, like the
   outbox's per-payment sequence) is the crux of the design.
3. **Virtual vs wall clock**: expiry is decided on virtual time; leases,
   deadlines and retries stay on wall time (D-052 precedent — one clock per
   timestamp, never mixed).
4. **Second inbound**: the sim → server direction is new authority — token,
   schema validation, replay window and bounded bodies borrow the webhook
   receiver's discipline, not the open POST surface.
5. **Scope creep**: the inbox must not grow settlement semantics in this
   slice; only the expiry outcome rides it.

## END_GOAL claims the branch has not yet proven

| Claim | State |
|---|---|
| Post-load reconciliation "zero mismatches" | Proven on the four **passing** full runs (m0, m4, m6 re-run, m7). The m2-lifecycle and m3-uncertain misses left their gates partially unmeasured or under a failing run; re-runs owed — m5-explain at 60 req/s and m8-overload/probe-read-edge/m2-lifecycle are in flight on `31ff704` (dispatches 44878700774, 44878702640) |
| "100 ops/s sustained, 5× burst, hot-account + hot-payment races" at million scale | m1-auth, m2-lifecycle, m3-uncertain, m5-explain full runs **missed** their thresholds (kept, not rerun-till-green); the owed re-runs above close the correctness evidence, the rate findings stay open |
| Owed runs named in ROADMAP | m0 transport-limit run; m4 black-hole saturation run; m5 report-budget (429-under-saturation) run; reconciler defaults (D-048) measured run |
| Explain under load | Functional proof complete (`every_balance_is_explained…`, sealed cursors, drift detection); the m5-explain full run missed the offered rate (298/2,500 reads/s) — the D-096 derived-balances re-run is the evidence in flight |
| Dev store tests failures reproducibly | Proven: 11 scenario traces run twice and after a reset, diffed (`every_library_scenario_runs_twice…`) |
| Recovery without a second charge or fulfillment | Proven: drills a–c, checkout-path kills, delivery fuzzer |
| Live-certificate-Authority proxy (D-074); fresh-machine install by a person | Owed (M7) |
| M8 settlement→payouts→disputes→invoices→recurring; x402 | Full direction — needs the M8.1 inbox first |
