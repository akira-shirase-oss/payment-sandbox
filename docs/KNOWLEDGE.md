# Knowledge — findings register

The security review's ledger: what was found, its severity, its status, and where it
is decided. S1 and S2 block the merge; S3 is tracked here until it is decided or
fixed. DECISIONS.md records the *chosen* design; this file records *what is still
open*.

## Resolved this round (fixes in this branch)

| ID | Finding | Fix | Proof |
|----|---------|-----|-------|
| S1-1 | Key cache could resurrect a revoked key or a pre-reset epoch for up to the 30 s TTL (miss → resolve → `put` raced `invalidate_*`); writes bound the cached epoch without checking `projects.dataset_epoch` | Generation counter on `KeyCache` + `put_if_unchanged`; `ensure_epoch` (`FOR KEY SHARE`) inside every write `begin` | `auth::tests::a_resolution_older_than_an_invalidation_is_never_cached`; `journal.rs::a_write_scoped_to_an_archived_epoch_is_refused` |
| S2-1 | `PgListener::recv()` reconnects silently, dropping the gap's notifications; `split_at(1)` panicked on empty/multibyte payloads; restart never flushed | `try_recv()` loop; `Ok(None)`/`Err` → flush + re-attach; flush after `listen()` lands; `strip_prefix` parse, malformed → `clear()` | `keys.rs::a_revocation_committed_in_a_listener_gap_still_flushes`; `authlisten::tests::a_malformed_notification_flushes_the_cache_without_panicking` |
| S2-2 | Over-pending refusal wrote a raw 503 then dropped the socket → RST → `status:0` (~1,620 in run `44835402763`, breaking m8-overload's contract) | Lingering close: response → `shutdown(write)` → ≤1 s drain | `sandbox-http::a_connection_past_the_pending_bound_is_refused_at_once` (whole response + EOF) |
| S2-3 | Hyper's ~408 KiB per-connection head buffer ≈ 209 MB per listener at the cap | `max_buf_size(32 * 1024)` | `sandbox-http::a_head_past_the_buffer_bound_is_refused` (16 KiB → 200, 64 KiB → 431) |
| S2-4 | Σ(cap+pending) over all listeners + pools could exceed `LimitNOFILE=4096` → EMFILE mid-run | Boot-time rlimit check (raise soft to need, refuse past hard); site caps 128 | `main.rs::descriptor_budget` (arithmetic exercised at boot; the hard-limit path needs a constrained `ulimit -n` to reproduce — documented, not unit-tested) |
| S3-1a | Signed-not-encrypted cursors exposed the cluster's system identifier and global XIDs — a cross-tenant side channel (in the explain response's own `generation`/`as_of` fields too, not only the cursor) | `letters(nonce ‖ ChaCha20-Poly1305(payload))`, context as AAD; response `generation`/`as_of` now opaque keyed labels (`CursorKey::label`) | `cursor.rs::the_sealed_form_reveals_nothing`; `attacks::forged_explain_cursors_are_refused_cleanly`; mutant `forged_explain_cursors_are_refused_cleanly` (AAD removed → cross-context reuse) dies |
| S3-1b | Cursors never expired | `issued_at` inside the sealed payload; `open` refuses past `PS_CURSOR_TTL_MS` (default 1 h) with `410 cursor_expired`; unreadable/backwards clock fails closed | `cursor.rs::expired_cursors_are_refused_by_name`; `attacks::expired_explain_cursors_are_refused_by_name`; mutant `expired_explain_cursors_are_refused_by_name` (lease check removed) dies |
| S3-2 | `getrandom` failure fell back to a fixed cursor key → forgeable cursors | No fallback: no `PS_SECRET_KEY` and no OS randomness → refuse to start | `cursor.rs::without_os_randomness_there_is_no_key` |
| S3-3 | `problems()` validated only the write pool's statement timeout against the request deadline | Read-pool acquire + statement and write-pool acquire checked the same way | `config.rs::every_pool_deadline_must_beat_the_request_deadline` |
| S3-4 | A timed-out body answered 413 "too large" | Error chain walked for `TimeoutError`/`Elapsed` → `408 body_timeout`; 413 stays for size | `attacks::oversized_and_slow_bodies_are_cut_off` (named code on the wire) |

## Tracked — S3 (recorded, not yet decided)

All five S3 items are decided and remediated above (D-105). Nothing remains
tracked at S3 in this round.

## Also verified this round (the "sound" list, re-confirmed)

- Explain binds project + epoch in both the SQL and the cursor (the AEAD's AAD).
- Every `/v1` and dashboard route selects its pool through `DbPool` (D-101).
- Capture/refund capacity reservations run under `FOR UPDATE` in one transaction.
- Webhook egress is loopback-only with re-resolution at delivery, checked-address
  connect, no redirects/proxy, 1 s/5 s deadlines, and HMAC over
  `id.timestamp.body` inside a 300 s window, constant-time.
- Cursor ChaCha20-Poly1305 fails closed on any tamper; `limit` clamped 1..=1000.
- The card guard scans decoded JSON before any field is parsed (D-019).
- The read pool's default transaction is `read only` (SQLSTATE 25006 on a write).
