# Roadmap

Milestone order follows the guide's four build steps: accounting core
(step 1 → M0), payment lifecycle (step 2 → M1–M2), separate simulator
(step 3 → M3), developer product (step 4 → M4–M7); the guide's "first
useful release" is M6, and M8 is the full-direction billing work. The
finer split (D-031) gives every milestone its own failure drills and load
scenario instead of leaving them for the end.

**Status rule.** A status cell may say *Done* only when it names the tests
and the evidence path that prove the gate, **including a committed full
load run** (workspace load-test standard): until then a milestone whose
functional gates pass is *In progress*. Every smoke run, failures included,
is listed in `perf/results/smoke/README.md`. Anything else is *Planned* or
*In progress*. Load evidence measured while the machine was busy is a
smoke observation, never capacity, and the full run stays listed as owed.

Every milestone ships: property tests that every journal stays balanced and
every balance equals its journal sum; the guide's failure cases that apply
to it as automated tests; a load scenario with thresholds under `perf/`.

| Milestone | Builds | Proves (gate) | Status |
|---|---|---|---|
| **M0 — the spine** | Cargo workspace; pure `sandbox-domain` (`Money` over D-010's currency table, chart of accounts, journal entries, posting recipes, operation state machine); PostgreSQL schema with an append-only, balance-checked journal; Axum skeleton (bounded bodies, deadlines, idempotency-key parsing, one error envelope); `scripts/check.sh`; CI; `deploy/compose.yaml` | Every posted entry balances per currency (property tests + a commit-time trigger); unbalanced, mixed-currency and overflowing postings are named refusals; `UPDATE`/`DELETE`/`TRUNCATE` on the journal fail in the database; the domain crate has no I/O dependencies; first load scenario and first failure drill (database outage) | In progress: functional gates pass; the full run passed on a GitHub-hosted runner (D-089); a transport-limit run is owed |
| **M1 — projects and sandbox keys** | Projects created with a bootstrap token; `sk_sim_` keys hashed, with scopes and revocation; project-scoped repositories over composite keys | A key of project A can never read or write project B (schema and API tests); a revoked key fails at once; secrets never appear in logs (log-capture test); load scenario on authenticated reads | In progress: functional gates pass; the full run missed its thresholds on a GitHub-hosted runner (D-089) |
| **M2 — payment lifecycle** | Payments, authorize / capture / void / refund as two-phase holds (D-011), operations with derived identity (D-015) and payload-bound keys (D-016), transactional capacity (D-014); `sandbox-sim` as a separate process with its own store (redb, D-034) answering approve/decline by scenario token; raw-card rejection (D-019) | Same key + different payload → conflict; the D-027 races (hot payment, hot account) cannot overrun capacity and the excess fails with a named refusal; capture-vs-void and refund-vs-pending-capture races; zero journal imbalance and every payment's sums equal its journal sums; PAN-like input rejected and never persisted or logged; load scenario: lifecycle at a fixed rate plus both races | In progress: functional gates pass; full runs on GitHub-hosted runners (D-089): `m2-lifecycle` missed, `m2-refund-race` passed |
| **M3 — uncertain outcomes and recovery** | Simulator lost replies, timeouts and seeded failure scenarios; fenced readback (D-017); reconciler at startup and periodically; reverse reconciliation; simulator virtual clock | Lost replies stay `uncertain` until readback, then apply exactly once; **drill (a)**: server killed after the simulator accepted, before the result was stored → recovers with no second charge; restart of either or both processes mid-operation; **drill (b, simulator half)**: a virtual-clock jump forward and back leaves posted journal history byte-identical; **drill (c)**: server database restored from an older dump, newer simulator store → a client retry reconciles with no second charge, and reverse reconciliation reports what the server lost | In progress: functional gates pass; the full run missed (its reconciliation gate timed out) on a GitHub-hosted runner (D-089) |
| **M4 — signed webhooks** | Outbox committed with every state change; leased deliverer with fencing, deadline, bounded jittered retries (D-032); signatures and rotation (D-020); egress guard (D-023); `fixtures/dev-store` (D-024) with inbox dedup, thin-event re-read (D-025) and a unique fulfillment key | Delivery fuzzer: random duplication, reordering and delay over N payments ends with fulfillments equal to captured payments and zero duplicates; receiver down → retries then delivers, nothing lost; signature, replay-window and rotation tests; loopback-only egress refuses private, link-local, public and redirect targets; **drill (b, wall-clock half)**: a lease-clock jump causes at most a duplicate delivery, never a lost or corrupt one | In progress: functional gates pass; the full run passed on a GitHub-hosted runner (D-089); a saturation run is owed |
| **M5 — reconciliation and explain-balance** | Reconciliation report (journal balance, payment sums vs journal, account balances against what the payments imply, open uncertain operations, agreement with the network both ways and field by field (D-041)); `explain-balance` for any account | After every load run, every account's balance equals the sum of its journal entries with zero mismatches; any balance can be explained line by line; a planted drift is detected | In progress: functional gates pass; the full run missed its thresholds on a GitHub-hosted runner (D-089) |
| **M6 — first useful release** | SvelteKit + Tailwind v4 + shadcn-svelte (D-035, from this repository's copy of the Colophon UI package in `packages/ui`) checkout (scenario picker, **no card field**, D-019) and inspection dashboard (operations, postings, balances, events, reconciler state) on separate origins (D-029) | The guide's first useful release end to end: one synthetic purchase, partial refund, balanced ledger visible in the UI, durable webhook fulfilled by the dev store, and the M3 kill drill repeated through the checkout path; browser tests prove no card field exists | In progress: functional gates pass (journeys and drills below); the `m6-checkout` full run missed its latency thresholds on a GitHub-hosted runner (D-089) |
| **M7 — developer product** | Rust CLI, TypeScript SDK, seeded scenario library, dataset reset as a new epoch with typed confirmation (D-022), sandbox labels end to end, migration/restore tests per release | A development store tests payment failures reproducibly: each library scenario run twice yields the same sequence of states and events (ids aside); reset needs `admin` + typed slug and never deletes journal rows; production-looking SDK config refused | In progress: functional gates pass (below); the `m7-developer` full run passed on a GitHub-hosted runner (D-089); a live certificate authority and a person's fresh-machine install are owed |
| **M8 — billing direction** | Server-side inbox for asynchronous simulator outcomes; authorization expiry on virtual time; then settlement → fees → payouts → disputes → invoices → recurring, each with its own contract | Each new scenario reconciles to the journal; virtual time never rewrites already-posted history | Planned (after v1.0.0, as v1.x slices) |
| **x402 — machine payments** | The x402 brief: HTTP 402 payment flows for agents, in simulated money, on the same journal | Its own contract, written when it starts | Planned (after v1.0.0) |

### M0 status — In progress (functional gates pass; full load run passed; a transport-limit run owed)

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
| Every listener bounded at the transport (review N1, D-054) | `tests/transport.rs` in the server, the simulator (70 silent connections under `ulimit -n 64`) and the store, against the real binaries; `sandbox-http`'s write-stall test |
| The journal's protections checked absolutely (review N5, D-058) | `the_server_refuses_a_database_whose_protections_were_weakened`; `the_runtime_role_cannot_choose_an_entrys_sequence_number` (N8) |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **pass**, 999,988 of 1,000,000 offered at 10,000/s, p99 2.4 ms (`perf/results/20261117T182128Z-m0-probes-full.json`) |
| **Owed** | A load run that sets the transport limits (D-054 defaults are chosen, not measured) |

### M1 status — In progress (functional gates pass; full load run missed)

| Gate | Evidence |
|---|---|
| Projects only through the operator's bootstrap token | `tests/keys.rs`: `creating_a_project_needs_the_operator_bootstrap_token`, `without_a_configured_bootstrap_token_nobody_can_create_projects`, `slugs_are_unique_and_validated` |
| Scopes; live-looking and malformed keys refused | `scopes_decide_what_a_key_may_do`, `malformed_and_live_looking_keys_are_refused` (`sk_live_`/`sk_test_` never authenticate) |
| A revoked key fails at once; no lock-out | `a_revoked_key_fails_on_its_very_next_request`; `the_last_admin_key_cannot_be_revoked_even_by_two_concurrent_requests` (two admins revoking each other concurrently: exactly one succeeds) |
| Project A can never see or touch project B | `projects_cannot_see_or_touch_each_others_keys`; schema: composite keys on every table (`0002_keys.sql`), cross-project journal references refused (`currencies_accounts_and_projects_cannot_be_mixed`) |
| Secrets never in logs | `tests/logs.rs`: `no_secret_ever_reaches_the_logs` — every event at TRACE from server, Tower and sqlx, with a canary proving the capture works; a planted leak of the key made it fail (mutation check, reverted) |
| Load scenario | `perf/scenarios/m1-auth.js` (hash + key lookup + project read per request). Smoke 500/s × 20 s on `bbb8104` + working tree: 10,005 requests, 0 failed, p99 21.4 ms — **machine busy (load average 130–256), a smoke observation only** |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **miss**, 3,734 of 5,000 reads/s served on 4 vCPUs shared with the generator, p95 155 ms, 0.41% failed (`perf/results/20261117T182157Z-m1-auth-full.json`) |
| **Owed** | A full run that meets its thresholds (a larger runner, or a decision on the per-request key cost) |

### M2 status — In progress (functional gates pass; lifecycle full run missed, refund race passed)

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
| **Full runs** | GitHub-hosted runners, 2026-11-17 (D-089): `m2-lifecycle` **miss**, 1,063,905 requests, 9.5% failed under the bursts, p99 13.8 s, reconciliation clean (`perf/results/20261117T182142Z-m2-lifecycle-full.json`); `m2-refund-race` **pass** at its defined size (36,000 offered, below the 1,000,000 rule by design), refund p99 8 ms (`perf/results/20261117T182143Z-m2-refund-race-full.json`) |
| **Owed** | An `m2-lifecycle` full run that meets its thresholds; its latency thresholds are still to be decided by a run on a larger or quieter machine |

### M3 status — In progress (functional gates pass; full load run missed)

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
| `pg_dump` logical-restore rehearsal (D-026, D-053) | `scripts/restore-rehearsal.sh` (fingerprint of source and copy identical; invariants on the copy); `tests/restore.rs`: `a_logical_restore_is_the_same_ledger_with_the_same_protections`; a cursor from before a restore is refused (`an_explanation_cursor_from_before_a_restore_is_refused`, D-050) |
| Fair, concurrent, backed-off recovery (review 11, D-048) | `tests/reconciler.rs`: one project's backlog cannot starve another's; a failed check backs off; readbacks run concurrently within their bound |
| Supervised background tasks (review 19, D-051) | `supervisor::tests`; drill `a_panicking_reconciler_is_restarted_and_still_resolves_uncertain_operations` |
| A restore into another cluster (review N2, D-055) | `a_restore_into_another_cluster_explains_the_same_and_keeps_posted_entries_closed` (second cluster `postgres-restore`, 56450, local and CI); the rehearsal fails on a weakened source (`the_rehearsal_fails_when_the_source_itself_was_weakened`, D-058) |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **miss**, 1,836,007 requests, load thresholds met, but the end-of-run wait for the reconciler hit `teardown()`'s 180 s limit, so the reconciliation gate was not measured (`perf/results/20261117T182149Z-m3-uncertain-full.json`) |
| **Owed** | A teardown limit above its own wait and fewer metric tags in `m3-uncertain.js`, then a new full run; the reconciler's defaults (D-048) are chosen, not measured |

### M4 status — In progress (functional gates pass; full load run passed; a saturation run owed)

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
| One endpoint cannot hold the deliverer (review 12, D-049); one clock per timestamp (review 20, D-052) | `a_black_holed_endpoint_cannot_hold_the_deliverers_capacity`; `an_endpoint_that_keeps_failing_opens_its_circuit_and_is_probed_once`; `a_new_delivery_is_due_at_once_whatever_the_deliverers_clock_says` |
| The network's and the store's internal routes need credentials (review 10, D-046) | `sandbox-sim/tests/access.rs`, `fixtures/dev-store/tests/control.rs` (the real binaries); `another_projects_authorization_cannot_be_captured_voided_or_refunded` |
| Per-project delivery fairness and an endpoint cap (review N4, D-057); webhook target ports (N10, D-059) | `a_project_with_many_black_holed_endpoints_cannot_hold_the_deliverer`; `endpoints_are_capped_per_project_and_listed_in_pages`; `webhooks_may_not_target_the_sandboxs_own_ports_or_unlisted_ones` |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **pass**, 1,000,093 API requests with every delivery, p99 49.5 ms, 13 lock waits over 50 ms (`perf/results/20261117T182144Z-m4-webhooks-full.json`) |
| **Owed** | A saturation run of black-holed endpoints beside healthy ones (the fairness above is tested, not measured under load; D-049 and D-057 defaults are chosen, not measured) |

### M5 status — In progress (functional gates pass; full load run missed)

| Gate | Evidence |
|---|---|
| The report: journal balance per currency, payment sums vs journal, one entry per succeeded operation, account balances vs what the payments imply, open operations, the network both ways | `sandbox-db/src/reconcile.rs`; `GET /v1/reconciliation[?network=true]`; every payment, recovery, drill and webhook test ends with it clean |
| Any balance explained line by line | `GET /v1/balances/{currency}/{account}/explain` (pages by entry sequence, running totals); `tests/explain.rs`: `every_balance_is_explained_line_by_line_across_pages` (pages of 3; the lines sum to every balance) |
| A planted drift is detected | `a_planted_drift_is_detected_exactly` — 50 minor units of refund planted on one payment: the report names exactly that payment and the two accounts it moves, and is clean again once it is undone |
| After load runs, zero mismatches | the teardowns of `m2-lifecycle`, `m2-refund-race`, `m3-uncertain`, `m4-webhooks` and `m5-explain` require a clean report |
| Load | `perf/scenarios/m5-explain.js` smoke: 4,306 requests, balances p99 9 ms, explain p95 14 ms / p99 26 ms, each account's explanation walked to its end summing exactly to its balance, report clean — passed, machine busy (load average 22–29), a smoke observation |
| Reports are bounded per project (review 9, D-047) | `tests/reports.rs`: `reports_are_rate_limited_per_project`; `reports::tests`; the network's listings run outside its writer (`listings_and_the_clock_are_served_while_the_writer_is_busy`) |
| Sealed explanation cursors, card-guarded query strings (review N3, D-056) | `a_cursor_the_server_did_not_issue_is_refused_before_any_query`; `tests/logs.rs`; `cursor::tests` |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **miss**, 298 reads/s served of 2,500 offered over 100,002 postings, 60% failed, the explain gate held (`perf/results/20261117T182158Z-m5-explain-full.json`) |
| **Owed** | An `m5-explain` full run that meets its thresholds (the reports' cost on a 4-vCPU runner is the finding); a load run of the report budget itself (429 under saturation; D-047's defaults are chosen, not measured) |

### M6 status — In progress (functional gates pass; full load run missed on latency)

Decisions: D-060 (checkout sessions), D-061 (the two origins and their policies), D-062
(dashboard sign-in and CSRF), D-063 (simulator controls), D-064 (size budgets), D-065 (the
review's fixes).

| Gate | Evidence |
|---|---|
| One synthetic purchase end to end through the checkout, a partial refund, a balanced ledger visible in the UI, a durable webhook fulfilled by the dev store | Journey `a purchase is paid, fulfilled by the dev store, partly refunded and balanced in the dashboard` (`e2e/tests/checkout.spec.ts`, `scripts/e2e.sh`): the page shows Paid; the dev store fulfills the payment exactly once; the merchant refunds 12.00 of 42.00; the dashboard shows Clean, the payment Part refunded with its three operations, and the payer's page the refund; `?network=true` reconciliation clean |
| The M3 kill drill repeated through the checkout path | `tests/drills.rs` (the real binary): `drill_a_through_the_checkout_a_payer_whose_server_died_mid_payment_is_charged_once` (killed after the network approved the authorization: the restarted reconciler applies it, the page continues, one authorization and one capture at the network, two journal entries) and `drill_a_through_the_checkout_a_capture_lost_to_a_crash_is_recorded_once` |
| Browser tests prove no card field exists | `there is no card field anywhere in the checkout` (every input on every checkout page is a scenario radio; no `cc-*` autocomplete, no card words); `tests/checkout.rs` refuses card data in attempt bodies |
| Never charged twice; truthful states | Journeys: decline then retry; uncertain then reconciled (the sampled states never show Paid before the last Uncertain); a lost response plus a double click retried under the same key (one attempt, one payment); `tests/checkout.rs` (9 tests: same key re-drives, another key is `checkout_attempt_open`, same key and another scenario is a conflict, uncertain is never paid, fenced is "not charged", bounded attempts, expiry); `sandbox-domain` `checkout::tests` |
| Separate origins, strict policies, embedding only where named | Journeys: light and dark mode under the policies with zero violations (both sites, every dashboard section); embedded checkout framed only from its session's origin, refused (`frame-ancestors 'none'`) otherwise, `postMessage` only to the named origin; `tests/checkout.rs` and `tests/dashboard.rs` pin the headers |
| Dashboard sessions and CSRF | Journey `signing in gives a host-only session cookie…` (`__Host-`, HttpOnly, Secure, SameSite=Strict; unreadable by the page; a state change without the CSRF token is 403; sign-out ends it on the server); `tests/dashboard.rs` (5 tests: Origin and CSRF on every state change, cross-site reads refused, a doubled cookie trusted by neither copy, a revoked key ends its sessions at once, simulator controls only in dev and only with the operator's credential) |
| Explanations paged in the UI; long lists virtualized | Journeys `an explanation pages through the journal until its lines sum to the balance` (pages of three, at least three pages, Explained) and `the payments list stays bounded…`; `VirtualRows.test.ts` (10,000 rows render under 40) |
| Keyboard and phone | Journeys `the checkout can be paid with the keyboard alone` (visible focus) and `at 360 px…` (no sideways scroll, every badge whole) |
| Component tests | Vitest: `packages/ui` (money, outcome states, badge, token contrast in four schemes), `apps/checkout` (flow, picker, status), `apps/dashboard` (virtual list) |
| Size budgets and page loads | `scripts/web-budget.mjs` in `scripts/check.sh` and CI (D-064); `perf/results/*-web-m6.json` (`scripts/web-perf.sh`) |
| Review fixes (D-065) | A restore cannot lead to a second charge: a network identity conflict closes the session (`after_a_restore_a_conflicting_attempt_closes_the_session_and_never_charges_twice`, restored into the second cluster; `after_a_restore_paying_again_recovers_the_payment_without_a_second_charge`); the dashboard answers on its own host name and needs fetch metadata or its Origin on reads (`reads_need_fetch_metadata_or_this_origin_even_with_the_cookie`, `the_dashboard_answers_only_on_its_own_host_name`); at most 8 sessions per key |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **miss** on latency only, 999,061 requests, 0% failed, every session paid once and reconciled, p95 115 ms, p99 423 ms (`perf/results/20261117T182141Z-m6-checkout-full.json`) |
| **Owed** | An `m6-checkout` full run within its latency thresholds |

### M7 status — In progress (functional gates pass; full load run passed)

Decisions: D-066 (dataset reset), D-067 (the interface standard's tests), D-068 (clients:
configuration, retries, honest answers; the CLI), D-069 (the scenario library), D-070 (the
TypeScript SDK), D-071 (the production path), D-072 (migration and restore tests per
release), D-073 (labels end to end; the load scenario); and the v1 readiness fixes, D-074 to
D-088 (below the table).

| Gate | Evidence |
|---|---|
| Reset needs `admin` + the typed slug and never deletes journal rows | `tests/reset.rs`: `a_reset_needs_admin_and_the_typed_slug_and_deletes_nothing` (every journal row of every epoch byte-identical before and after; the archived epoch still reconciles), `the_database_lets_a_project_change_only_by_one_epoch_forward` (`PSP01` for the runtime role and the owner), `concurrent_resets_of_one_epoch_open_exactly_one_new_epoch`, `an_operation_open_across_a_reset_resolves_in_its_own_epoch`, `cursors_and_checkouts_of_an_archived_epoch_are_refused`; the dashboard's reset (`the_dashboards_reset_needs_admin_the_typed_slug_origin_and_csrf`; journeys in `e2e/tests/dataset.spec.ts`) |
| Each library scenario run twice yields the same states and events (ids aside) | `crates/sandbox-cli/tests/scenarios.rs`: `every_library_scenario_runs_twice_and_after_a_reset_with_the_same_trace` — 11 scenarios (`fixtures/scenarios/`), each run twice in one project and once more after a reset under the first run's keys, every trace equal to the committed one (`fixtures/scenarios/expected/`); the development store fulfills exactly the payments that captured money, once each; reconciliation with the network clean |
| Rust CLI, retrying safely | `crates/sandbox-cli`; `tests/cli.rs`: a developer's session through the binary (every exit status), production-looking configuration refused before anything is sent (plain `http://` off loopback too, D-079), a lost response retried under the same key charging once (a proxy drops the first answer), busy answers retried under one key and bounded, server and gateway errors on money-moving calls reported as unknown with the key (D-075) and a server restart behind a proxy charging once, the deadline never overrun and plain POSTs never re-sent after a possible commit (D-078), checkout sessions and webhook endpoints (D-084); `tests/tls.rs`: the CLI over HTTPS through a TLS proxy with a test CA, and each TLS failure in plain words (D-074); `sandbox-cli scenarios run --all --check` |
| TypeScript SDK; production-looking SDK config refused | `packages/sdk-ts`; `test/unit/config.test.ts` (a real processor's key or host, a remote host without `allowNonLocal`, plain `http://` off loopback without `allowPlainHttp`, `NODE_ENV=production` without `allowProductionEnvironment`: refused before anything is sent, the key never echoed); `test/unit/client.test.ts` (retries under one key, bounded by the deadline, honest unknown on no answer, a server error or a gateway's answer; resets never re-sent after a possible commit; checkout sessions and webhook endpoints); `test/live/sdk.test.ts` against the real server (6, in `scripts/e2e.sh`); webhook vectors shared with the Rust signer |
| Developer docs whose examples run; an API reference that matches the routes | `docs/DEVELOPERS.md` (the CLI, the SDK, the hosted checkout, webhooks, the library, reset); `examples/cli-quickstart.sh`, `sdk-purchase.mjs`, `checkout.mjs` (paid by a stand-in payer) and `webhook-receiver.mjs` run as written by `scripts/e2e.sh`; `scripts/check-doc-examples.mjs`; `docs/API.md`, kept equal to `routes.rs` and the error codes by `tests/api_reference.rs` (D-084) |
| Sandbox labels end to end | every binary's `--version` (`tests/labels.rs` in the server and the network, the CLI's `help_and_version_carry_the_notice...`); `Sandbox-Simulated: true` required by both clients; the notice in the UI banners, the env templates, the units and every README |
| Migration/restore tests per release | `tests/releases.rs`: `every_releases_data_restores_and_migrates_to_this_release` restores `fixtures/releases/1.0.0.json` (every row of a 1.0.0 database: two projects, purchases, a decline, a resolved lost reply, a void, refunds, webhook events, a checkout session, a reset) into a database at its migration, applies the rest, and requires the role and protections check, every epoch reconciled with the same balances, an old key replaying, new money moving; a breaking migration fails it (mutation check: a `CHECK` violated by the fixture's rows, then removed); the fixture is at the newest migration, so coverage starts with the first migration after v1.0.0 (D-087) |
| A tested production path (D-071) | `deploy/systemd/` (three services, the backup service and its timer), `deploy/env/*.env.example`, `deploy/Caddyfile`, `deploy/backup/`, `docs/OPERATING.md` (install, first run, upgrade, backup and restore, troubleshooting); `scripts/release-smoke.sh` (release builds from the templates in the `shared` profile with public origins, the CLI quickstart as written, the API behind a pinned Caddy with its local CA and the CLI paying over `https`, D-074) locally and in CI's `release` job, which then installs the units under systemd against PostgreSQL 18 from Ubuntu packages (`scripts/ci-postgresql.sh`; `setup` over the Unix socket as `postgres`, the old passwordless form refused, D-082), checks that no service user can read an env file (D-081), proves a restart by a new PID, and backs up and restores (the payment found, `reconcile --network` clean, D-083) (`scripts/release-systemd.sh`); `roles::tests` (setup never resets a password), `config::tests::the_sites_may_have_public_https_origins_only_in_the_shared_profile` |
| The interface standard | `e2e/tests/a11y.spec.ts` (axe, every screen of both sites at 360 and 1440 px, light and dark: zero serious or critical, D-085); `e2e/tests/dashboard.spec.ts` (errors in plain words with a next step, foreign keys refused in the page, D-086); `e2e/tests/vitals.spec.ts` (LCP, CLS and INP of the production builds within the good thresholds, D-088; recorded in `perf/results/20261117T175336Z-web-v1.json` with the gzip sizes); 23 screens and states, 92 screenshots in `docs/screens/` (`e2e/tests/screens.spec.ts` via `scripts/screens.sh`) |
| Load scenario | `perf/scenarios/m7-developer.js` (D-073): written and parsed with `k6 inspect`, thresholds per step and a reconciliation gate; **not run**, smoke or full |
| **Full run** | GitHub-hosted runner, 2026-11-17 (D-089): **pass**, 1,008,135 requests, 0% failed, p99 21 ms, every project's epoch reconciled (`perf/results/20261117T182205Z-m7-developer-full.json`) |
| **Owed** | The proxy against a live certificate authority (CI runs Caddy with its own local CA only); the operator guide followed on a fresh machine by a person |

### The v1 readiness review (2026-11-17) and its fixes

The independent review of `895a421` answered "fix first". Every finding is fixed with a test
that failed before the fix (or, for scripts that only CI can run, a check shown failing on
the old form), recorded in D-074 to D-088:

| Finding | Fix | Decision |
|---|---|---|
| H1: the CLI had no TLS | rustls, `--ca-cert`, plain TLS errors; Caddy over https in the release smoke | D-074 |
| H2: a 5xx or a gateway's answer on a money-moving call was "refused" or "not a sandbox" | no answer: retried under the key, then unknown naming it (both clients) | D-075 |
| M1: a torn payment read | one REPEATABLE READ snapshot for the payment, its operations and its journal | D-076 |
| M2: axe at 360 px; the ledger's balance hidden | labelled, focusable table regions; the balance column kept; axe at 360 and 1440 | D-085 |
| M3: no checkout or webhook endpoints in the clients and docs; no API reference | SDK and CLI support, a Checkout section and example run in CI, `docs/API.md` tested against the routes | D-084 |
| M4: the server's user could read the owner's password | env files `0600 root`, migrations as a dynamic user, asserted in the release job | D-081 |
| M5: no path to a key from the README; the wrong refusal | README "Run it" to a first payment; `bootstrap_token_required` | D-077 |
| M6: missing screenshots | every screen and key state at 360/1440, light and dark | — |
| M7: manual backups | a nightly backup service and timer (live dump, the network's store, the env files) and a tested restore | D-083 |
| M8: an admin URL that fails on stock Ubuntu PostgreSQL | the Unix-socket form, run in CI against PostgreSQL 18 packages | D-082 |
| L1, L2 | attempts capped by the deadline; plain POSTs never re-sent after a possible commit | D-078 |
| L3 | `https://` required off loopback unless `--allow-plain-http` | D-079 |
| L4 | dashboard errors in plain words with a next step; foreign keys refused in the page | D-086 |
| L5 | the developer guide's gaps; the webhook example run in CI | — |
| L6 | version 1.0.0 and its release fixture | D-087 |
| L7 | the release job proves a restart and uses the operator's passwords | — |
| L8 | `sandbox-dev-store --version` and `--help` | D-080 |

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
