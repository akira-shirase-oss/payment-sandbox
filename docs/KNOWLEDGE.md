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

## Tracked — S3 (recorded, not yet decided)

- **Explain cursors are signed but not encrypted.** The sealed payload exposes the
  cluster's PostgreSQL system identifier and global XIDs (`pg_control_system()`,
  snapshot `xmin:xmax:xip`) — a cross-tenant side channel on system identity and
  write rate. Mitigation to decide: encrypt the payload (AEAD under the same derived
  key), or bind a server-side nonce. Currently accepted as a leak of *system
  metadata*, never of tenant rows: the cursor's content is all-or-nothing refuse.
- **Cursors never expire.** An old `as_of` makes an explain page scan from its own
  snapshot forward; bounded by the 1 s read timeout and the report budget (D-047).
  A TTL inside the sealed payload is the obvious fix when this becomes load, not a
  leak.
- **`getrandom` failure falls back to a fixed cursor key** (cursor.rs): if OS
  randomness fails at boot, issued cursors are forgeable against the well-known
  fallback. Decide: refuse to boot instead — the fallback exists so a degraded
  environment still *starts*; boot refusal is the safer default for a security
  boundary.
- **Read-pool timeouts are not validated against the request timeout**
  (config.rs validates only the write pool): a read acquire longer than the request
  deadline could answer after the caller's own timeout. The bulkhead's defaults are
  consistent today; the check should be symmetric.
- **A timed-out body answers 413 "too large"** (extract.rs): the status names the
  wrong cause (a 408 or 503 would be honest). Attack 6 asserts the current named
  refusal; changing the status is a small breaking change — decide at the next API
  review.

## Also verified this round (the "sound" list, re-confirmed)

- Explain binds project + epoch in both the SQL and the sealed cursor.
- Every `/v1` and dashboard route selects its pool through `DbPool` (D-101).
- Capture/refund capacity reservations run under `FOR UPDATE` in one transaction.
- Webhook egress is loopback-only with re-resolution at delivery, checked-address
  connect, no redirects/proxy, 1 s/5 s deadlines, and HMAC over
  `id.timestamp.body` inside a 300 s window, constant-time.
- Cursor HMAC-SHA256/128 compared constant-time; `limit` clamped 1..=1000.
- The card guard scans decoded JSON before any field is parsed (D-019).
- The read pool's default transaction is `read only` (SQLSTATE 25006 on a write).
