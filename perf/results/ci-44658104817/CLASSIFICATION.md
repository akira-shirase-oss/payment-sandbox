# CI run 44658104817 — failure classification

Run `44658104817` (`load-full.yml`), code under test `be5823db` — before the D-100
database bulkhead (`2912c53`). GitHub-hosted runner: 4 cores shared by the load
generator, the server, the simulated network and PostgreSQL.

Every failed request is in exactly one class:

- **(a) test-script bug** — the scenario counted an expected domain answer as a
  failure;
- **(b) product bug** — a 500, a wrong status, or a block on a path that must not
  block;
- **(c) capacity** — a named refusal (503 + `Retry-After`) or a client-side timeout
  under saturation on the shared 4-core runner.

Counts are from `k6_summary`/`op_status` in the committed result JSONs beside this
file.

## m2-lifecycle — 863,985 requests, failed_rate 0.250

| op | status | count | class | why |
|---|---|---|---|---|
| authorize | 503 | 213,146 | (c) | named `Overloaded`/pool-deadline refusals; PostgreSQL used 2,010 s of the 3,013 s of CPU on the runner |
| capture | 503 | 727 | (c) | same |
| refund | 503 | 1,226 | (c) | same |
| other | 503 | 2 | (c) | same |
| authorize/capture/refund | 0 | 648 | (c) | client 2 s deadline hit under the same saturation; p99 ≈ 2,004 ms pins the client timeout |
| refund | **409** | **78** | **(a)** | the script fired `refund` without checking the capture's status; when a capture was shed, the refund ran against `captured=0` and the domain honestly answered `refund_exceeds_captured` |

Total explained: 215,827 ≈ 24.98%.

**Fix (a):** `m2-lifecycle.js` now early-returns on a failed capture, matching the
authorize gate — the honest 409 is no longer counted as a product failure. Red: the
78 409s in this run; green: the next m2 run shows `refund{status:409} = 0`.

## m3-uncertain — 1,614,347 requests, failed_rate 0.172

| op | status | count | class | why |
|---|---|---|---|---|
| approve | 503 | 222,116 | (c) | named refusals under saturation |
| uncertain | 503 | 54,761 | (c) | same |
| other | 503 | 2 | (c) | same |

Total explained: 276,879 ≈ 17.15%. **Zero 5xx, zero timeouts, zero wrong statuses** —
every failure is a named refusal. The 15.4 s max is a request that waited inside a
bounded path and still answered; nothing hit the client deadline (no `status:0`
recorded). 1,986 dropped iterations are the load generator itself running out of
capacity — also (c), on the generator side.

## m5-explain — 1,089,295 requests at ~1,204 req/s, failed_rate 0.630

| op | status | count | class | why |
|---|---|---|---|---|
| balances | 503 | 342,926 | (c) | reads shed at their own admission budget (D-099 edge): offered rate exceeded what 4 cores could serve — CPU saturated 44% of the run |
| explain | 503 | 343,324 | (c) | same |
| balances/explain | 0 | 31 | (c) | client timeouts under the same saturation |
| other | — | 100,205 ok | — | setup-phase writes, all succeeded |

Total explained: 686,281 ≈ 63.0%. p99 415 ms — refusals came back fast and named
(the read edge's bounded waits working as designed), not at the old ~2 s admission
plateau.

## Product-bug column

(b) is **empty in all three runs**: zero 5xx anywhere; every refusal carried
`Retry-After`. The only wrong-status candidate — m2's refund 409s — is the domain
behaving correctly and the script miscounting it (a).

## Does D-100 (`2912c53`) change any of this?

- **(a)** — unaffected; it was a script bug, fixed in the script.
- **(b)** — nothing to change.
- **(c)** — the *mechanism* changes, the counts mostly don't. These three scenarios
  failed at CPU-bound admission, not at a contended shared pool: m2/m3 are write
  floods (their 503s come from admission and the write pool under load), and m5's
  load phase is pure reads, so the shared pool was never contested by concurrent
  writes. What D-100 does change: under a mixed flood, reads can no longer wait on
  write-held connections — the residual the D-099 probes measured (~1 s
  acquire-floor refusals) is gone. One honest side effect: the write pool went from
  32 to 24 connections, so m2/m3 refusal counts may rise slightly on the next run;
  that is the bulkhead's price, not a regression to chase.

## What to re-dispatch

1. **m2-lifecycle** on the new commit — expects `refund{status:409}` = 0 and the
   failure rate to drop by roughly the 409 share (the 503 bulk stays: it is capacity).
2. **m3-uncertain**, **m5-explain** — baseline re-runs; expect refusal counts in the
   same class, now against split pools.
3. A **mixed read/write saturation** scenario (m2-shaped writes + m5-shaped reads on
   the main listener) if you want the bulkhead's benefit to show as a number — the
   three existing scenarios don't exercise it.
