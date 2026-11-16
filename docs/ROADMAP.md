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
| **M0 — the spine** *(Done)* | Cargo workspace; pure `sandbox-domain` (`Money` over D-010's currency table, chart of accounts, journal entries, posting recipes, operation state machine); PostgreSQL schema with an append-only, balance-checked journal; Axum skeleton (bounded bodies, deadlines, idempotency-key parsing, one error envelope); `scripts/check.sh`; CI; `deploy/compose.yaml` | Every posted entry balances per currency (property tests + a commit-time trigger); unbalanced, mixed-currency and overflowing postings are named refusals; `UPDATE`/`DELETE`/`TRUNCATE` on the journal fail in the database; the domain crate has no I/O dependencies; first load scenario and first failure drill (database outage) | See below |
| **M1 — projects and sandbox keys** *(Done)* | Projects created with a bootstrap token; `sk_sim_` keys hashed, with scopes and revocation; project-scoped repositories over composite keys | A key of project A can never read or write project B (schema and API tests); a revoked key fails at once; secrets never appear in logs (log-capture test); load scenario on authenticated reads | See below |
| **M2 — payment lifecycle** *(Done; full load runs owed)* | Payments, authorize / capture / void / refund as two-phase holds (D-011), operations with derived identity (D-015) and payload-bound keys (D-016), transactional capacity (D-014); `sandbox-sim` as a separate process with its own SQLite store answering approve/decline by scenario token; raw-card rejection (D-019) | Same key + different payload → conflict; the D-027 races (hot payment, hot account) cannot overrun capacity and the excess fails with a named refusal; capture-vs-void and refund-vs-pending-capture races; zero journal imbalance and every payment's sums equal its journal sums; PAN-like input rejected and never persisted or logged; load scenario: lifecycle at a fixed rate plus both races | Planned |
| **M3 — uncertain outcomes and recovery** *(Done; full load run owed)* | Simulator lost replies, timeouts and seeded failure scenarios; fenced readback (D-017); reconciler at startup and periodically; reverse reconciliation; simulator virtual clock | Lost replies stay `uncertain` until readback, then apply exactly once; **drill (a)**: server killed after the simulator accepted, before the result was stored → recovers with no second charge; restart of either or both processes mid-operation; **drill (b, simulator half)**: a virtual-clock jump forward and back leaves posted journal history byte-identical; **drill (c)**: server database restored from an older dump, newer simulator store → a client retry reconciles with no second charge, and reverse reconciliation reports what the server lost | Planned |
| **M4 — signed webhooks** *(Done; full load run owed)* | Outbox committed with every state change; leased deliverer with fencing, deadline, bounded jittered retries (D-032); signatures and rotation (D-020); egress guard (D-023); `fixtures/dev-store` (D-024) with inbox dedup, thin-event re-read (D-025) and a unique fulfillment key | Delivery fuzzer: random duplication, reordering and delay over N payments ends with fulfillments equal to captured payments and zero duplicates; receiver down → retries then delivers, nothing lost; signature, replay-window and rotation tests; loopback-only egress refuses private, link-local, public and redirect targets; **drill (b, wall-clock half)**: a lease-clock jump causes at most a duplicate delivery, never a lost or corrupt one | Planned |
| **M5 — reconciliation and explain-balance** *(Done; full load run owed)* | Reconciliation report (journal balance, payment sums vs journal, account checkpoints, open uncertain operations, simulator agreement both ways); `explain-balance` for any account | After every load run, every account's balance equals the sum of its journal entries with zero mismatches; any balance can be explained line by line; a planted drift is detected | Planned |
| **M6 — first useful release** | SvelteKit + Tailwind v4 + shadcn-svelte (D-035, from this repository's copy of the Colophon UI package in `packages/ui`) checkout (scenario picker, **no card field**, D-019) and inspection dashboard (operations, postings, balances, events, reconciler state) on separate origins (D-029) | The guide's first useful release end to end: one synthetic purchase, partial refund, balanced ledger visible in the UI, durable webhook fulfilled by the dev store, and the M3 kill drill repeated through the checkout path; browser tests prove no card field exists | Planned |
| **M7 — developer product** | Rust CLI, TypeScript SDK, seeded scenario library, dataset reset as a new epoch with typed confirmation (D-022), sandbox labels end to end, migration/restore tests per release | A development store tests payment failures reproducibly: each library scenario run twice yields the same sequence of states and events (ids aside); reset needs `admin` + typed slug and never deletes journal rows; production-looking SDK config refused | Planned |
| **M8 — billing direction** | Server-side inbox for asynchronous simulator outcomes; authorization expiry on virtual time; then settlement → fees → payouts → disputes → invoices → recurring, each with its own contract | Each new scenario reconciles to the journal; virtual time never rewrites already-posted history | Planned |

### M0 status — Done, with one owed load run

| Gate | Evidence |
|---|---|
| Every posted entry balances per currency | `sandbox-domain`: `journal::every_accepted_entry_sums_to_zero`, `recipe::every_recipe_entry_balances`, `payment::journal_and_capacity_stay_consistent` (property tests); `sandbox-db/tests/journal.rs`: `random_postings_always_balance_and_match_their_model` (property test against PostgreSQL) |
| Unbalanced, mixed-currency, overflowing postings are named refusals | `journal::invalid_postings_are_named_refusals`; `unbalanced_single_and_late_lines_are_refused_even_from_raw_sql` (PSJ02/PSJ03); `currencies_accounts_and_projects_cannot_be_mixed` |
| Corrections only as reversing entries; the journal is append-only in the database | `updates_deletes_and_truncates_of_the_journal_are_refused_by_the_database` (PSJ01 for UPDATE, DELETE, TRUNCATE, TRUNCATE CASCADE); `a_correction_is_a_reversing_entry_and_both_stay_in_history`; `posting_time_comes_from_the_database_clock_not_the_caller` |
| Crash mid-posting leaves nothing | `a_transaction_that_dies_mid_posting_leaves_nothing_behind` |
| Domain has no I/O | `scripts/check-architecture.sh` (and a self-test that it can fail) in `scripts/check.sh` and CI |
| Service skeleton: bounded bodies, deadlines, shedding, idempotency keys, card guard, one envelope | `sandbox-server/tests/skeleton.rs` (6 tests) + unit tests |
| First failure drill: database outage | `readiness_fails_fast_when_the_database_is_down_or_a_black_hole`: refused connections and a silent black hole both turn readiness red in < 2 s while liveness stays green |
| First load scenario | `perf/scenarios/m0-probes.js`. Smoke at 1,000/s × 20 s on commit `530e7ae` + working tree: 20,001 requests, 0 failed, p99 9.2 ms — **machine busy (load average 462 → see `perf/README.md`), a smoke observation only** |
| **Owed** | The full 1,000,000-request `m0-probes` run on a quiet machine, committed under `perf/results/` |

### M1 status — Done, with one owed load run

| Gate | Evidence |
|---|---|
| Projects only through the operator's bootstrap token | `tests/keys.rs`: `creating_a_project_needs_the_operator_bootstrap_token`, `without_a_configured_bootstrap_token_nobody_can_create_projects`, `slugs_are_unique_and_validated` |
| Scopes; live-looking and malformed keys refused | `scopes_decide_what_a_key_may_do`, `malformed_and_live_looking_keys_are_refused` (`sk_live_`/`sk_test_` never authenticate) |
| A revoked key fails at once; no lock-out | `a_revoked_key_fails_on_its_very_next_request`; `the_last_admin_key_cannot_be_revoked_even_by_two_concurrent_requests` (two admins revoking each other concurrently: exactly one succeeds) |
| Project A can never see or touch project B | `projects_cannot_see_or_touch_each_others_keys`; schema: composite keys on every table (`0002_keys.sql`), cross-project journal references refused (`currencies_accounts_and_projects_cannot_be_mixed`) |
| Secrets never in logs | `tests/logs.rs`: `no_secret_ever_reaches_the_logs` — every event at TRACE from server, Tower and sqlx, with a canary proving the capture works; a planted leak of the key made it fail (mutation check, reverted) |
| Load scenario | `perf/scenarios/m1-auth.js` (hash + key lookup + project read per request). Smoke 500/s × 20 s on `bbb8104` + working tree: 10,005 requests, 0 failed, p99 21.4 ms — **machine busy (load average 130–256), a smoke observation only** |
| **Owed** | The full 1,000,000-request `m1-auth` run on a quiet machine |

### M2 status — Done, with owed full load runs and a latency miss on a busy machine

| Gate | Evidence |
|---|---|
| The purchase path, two-phase holds, every currency scale | `tests/payments.rs`: `a_purchase_authorize_capture_partial_refund_balances_and_reconciles`, `zero_and_three_decimal_currencies_post_exact_minor_units`, `declines_are_recorded_and_a_declined_payment_cannot_be_captured` |
| Same key + different payload → conflict; same request replays, one network operation | `the_same_key_with_a_different_amount_is_a_conflict_and_the_same_request_replays` |
| D-027 races: excess fails with a named refusal | `sixty_four_concurrent_refunds_of_ten_against_three_hundred_admit_exactly_thirty` (exactly 30 / 34), `a_hot_merchant_account_takes_concurrent_refunds_across_many_payments` (120 / 40), `concurrent_captures_never_exceed_the_authorization` (6 of 10) |
| Capture vs void; refund vs a capture in flight | `a_void_racing_an_in_flight_capture_waits_for_it_and_then_releases_the_rest`, `a_refund_cannot_draw_on_a_capture_still_in_flight` |
| Capacity and finality in the schema | `sandbox-db/tests/capacity.rs`: raw SQL cannot overrun (23514) or reopen a resolved operation (PSO01) or delete (PSJ01) |
| Zero imbalance; every payment equal to its journal | every payment test ends with a clean `GET /v1/reconciliation`; `random_histories_keep_the_journal_balanced_and_every_payment_equal_to_its_entries` also checks every balance against the sum the payments imply |
| Raw card data rejected, never persisted or logged | `card_data_is_rejected_and_never_persisted` (no row, no text column, nothing at the network); `tests/logs.rs` |
| Project isolation, scopes | `one_project_cannot_read_or_move_anothers_payment`, `moving_money_needs_the_write_scope_and_a_key` |
| The simulator: one execution per identity, its own capacity checks, restart, clock | `sandbox-sim/tests/store.rs` (5 tests) |
| Uncertain answers stay uncertain; a retry re-drives the same identity | `an_unavailable_network_leaves_the_operation_uncertain_until_a_retry_resolves_it` |
| Regressions from the first race smoke (D-033) | `a_hot_payment_cannot_starve_the_pool_for_other_payments`, `recording_an_approval_survives_a_transient_lock_timeout` — each shown to fail without its fix |
| Load | `perf/scenarios/m2-refund-race.js` smoke: 2,065 requests, exactly 630 admitted / 1,370 refused / 0 shed, refund p99 39 ms, 0 lock waits over 50 ms, reconciliation clean — passed. `perf/scenarios/m2-lifecycle.js` smoke: 3,121 requests, 0 failed, reconciliation clean, **latency thresholds missed** (p95 512 ms, p99 635 ms against 100/250 ms) with the machine at load average 33–43 — both busy-machine observations on commit `d6228f5` + working tree |
| **Owed** | Full `m2-lifecycle` (≈1.17 M requests) and `m2-refund-race` runs on a quiet machine; the lifecycle latency thresholds are decided by that run |

### M3 status — Done, with an owed full load run

| Gate | Evidence |
|---|---|
| Lost replies stay `uncertain` until readback, then apply exactly once | `tests/recovery.rs`: `a_lost_reply_stays_uncertain_until_readback_and_then_applies_once`, `a_capture_whose_reply_is_lost_keeps_its_reservation_until_readback` |
| Fenced readback; reservations released only by a decline or a fence | `an_identity_the_network_never_executed_is_fenced_and_its_reservation_released` (a late delivery of the fenced identity is refused); `sandbox-sim/tests/store.rs`: `a_fenced_identity_refuses_every_later_arrival` |
| **Drill (a)**: server killed after the network accepted, before the answer was stored | `tests/drills.rs` (real binary, `fault-injection` build): `drill_a_server_killed_after_the_network_accepted_an_authorization_recovers_without_a_second_charge`, `drill_a_server_killed_after_the_network_accepted_a_capture_recovers_it_once`; killed before the call: `a_server_killed_before_the_network_call_fences_the_identity_and_releases_the_reservation` |
| Restart of either or both processes mid-operation | `the_networks_record_survives_its_restart_mid_operation`; `restarting_both_processes_mid_operation_applies_the_networks_record_once` |
| **Drill (b)**, simulator half: virtual-clock jumps never change posted history | `drill_b_virtual_clock_jumps_never_change_posted_history` (journal digest and network records identical after +90 and −180 days); `records_survive_a_restart_and_a_clock_jump_never_rewrites_them` |
| **Drill (c)**: older server backup restored, newer network store | `drill_c_restoring_an_older_server_backup_reconciles_without_a_second_charge` — reverse reconciliation reports exactly the 2 missing operations; client retries under the same keys re-derive the same identities; the network still holds 3 operations; the report is clean afterwards |
| Reverse-reconciliation paging (regression found by the first M3 smoke) | `reverse_reconciliation_pages_through_more_than_one_full_listing_page` — failed without its fix (a full 1,000-operation page exceeded the single-answer size cap) |
| Load | `perf/scenarios/m3-uncertain.js` smoke on `8fd1338` + working tree: 1,091 requests, 120 honest `202 uncertain` answers (lost replies and an unavailable network), every one resolved by the reconciler, both projects clean in both directions (0 network operations unknown to the server), approve-path p95 72 ms / p99 98 ms, 0 lock waits — **machine busy (load average 163–218), a smoke observation only** |
| **Owed** | The full `m3-uncertain` run on a quiet machine; a `pg_dump` logical-restore rehearsal script (D-026) |

### M4 status — Done, with an owed full load run

| Gate | Evidence |
|---|---|
| Outbox committed with every state change; one event per resolved operation, per-payment sequence | `0004_webhooks.sql` (`UNIQUE (project_id, operation_id)`, `UNIQUE (project_id, payment_id, sequence)`, events append-only); `a_purchase_is_delivered_signed_and_fulfilled_exactly_once` (sequences 1, 2, 3; a second deliverer pass claims nothing) |
| Signatures, replay window, rotation | `sandbox-domain::webhook` (7 tests); `forged_and_replayed_deliveries_are_refused_and_fulfill_nothing` (forged, 400 s old, tampered → 401); `a_rotation_keeps_consumers_on_either_secret_working` |
| Secrets encrypted at rest, bound to their row | `secrets::tests::a_sealed_secret_opens_only_for_its_own_row_and_key` |
| Loopback-only egress: private, link-local, public, names, credentials refused | `webhook_targets_must_be_loopback_and_webhooks_need_a_server_key`; domain `only_loopback_targets_are_allowed`; resolution re-checked at delivery, redirects not followed (`deliverer.rs`) |
| Receiver down → retries, then delivers, nothing lost | `a_receiver_that_is_down_gets_retries_and_then_every_event` |
| **Delivery fuzzer**: duplicates and reordering | `duplicated_and_reordered_deliveries_fulfill_each_captured_payment_exactly_once` — every event of 30 payments delivered 1–3 times in a shuffled order, concurrently; fulfillments equal exactly the payments with captured money; inbox equals the number of events |
| **Drill (b)**, wall-clock half | `drill_b_a_lease_clock_jump_causes_at_most_a_duplicate_delivery` — a jumped deliverer clock steals an expired lease and delivers again (one duplicate at the receiver); the stale worker's result is discarded by the fencing token; the row ends `delivered`, 1 attempt, no lease |
| `fixtures/dev-store` (D-024): inbox dedup, thin-event re-read, unique fulfillment key | `fixtures/dev-store/src/lib.rs`; every test above uses it as the consumer |
| Load | `perf/scenarios/m4-webhooks.js` smoke: 1,008 requests, 501 payments → 501 fulfillments, 1,002 events in the store's inbox, 0 rejected, 0 duplicates, reconciliation clean, 0 lock waits; API p95 120 ms (**threshold 100 ms missed**), p99 152 ms — machine busy (load average 33–38), a smoke observation. The first smoke found D-037 |
| **Owed** | The full `m4-webhooks` run (1,000,000 API requests and 1,000,000 deliveries) on a quiet machine |

### M5 status — Done, with an owed full load run

| Gate | Evidence |
|---|---|
| The report: journal balance per currency, payment sums vs journal, one entry per succeeded operation, account balances vs what the payments imply, open operations, the network both ways | `sandbox-db/src/reconcile.rs`; `GET /v1/reconciliation[?network=true]`; every payment, recovery, drill and webhook test ends with it clean |
| Any balance explained line by line | `GET /v1/balances/{currency}/{account}/explain` (pages by entry sequence, running totals); `tests/explain.rs`: `every_balance_is_explained_line_by_line_across_pages` (pages of 3; the lines sum to every balance) |
| A planted drift is detected | `a_planted_drift_is_detected_exactly` — 50 minor units of refund planted on one payment: the report names exactly that payment and the two accounts it moves, and is clean again once it is undone |
| After load runs, zero mismatches | the teardowns of `m2-lifecycle`, `m2-refund-race`, `m3-uncertain`, `m4-webhooks` and `m5-explain` require a clean report |
| Load | `perf/scenarios/m5-explain.js` smoke: 4,306 requests, balances p99 9 ms, explain p95 14 ms / p99 26 ms, each account's explanation walked to its end summing exactly to its balance, report clean — passed, machine busy (load average 22–29), a smoke observation |
| **Owed** | The full `m5-explain` run on the guide's 100,000 seeded postings and 1,000,000 reads, on a quiet machine |

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
