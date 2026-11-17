# Load evidence

**SIMULATED MONEY ONLY.** Benchmark traffic never leaves the machine: the server, the
database and (from M2) the simulated network all listen on loopback.

- `perf/scenarios/<name>.js` — k6 scenarios at a fixed arrival rate (offered load, not
  closed-loop concurrency). `MODE=full` is the workspace standard, ≥1,000,000 operations;
  `MODE=smoke` is a short run for CI and busy machines.
- `scripts/load.sh <name>` builds the release server, runs it on 127.0.0.1:56090 against
  `PS_DATABASE_URL`, runs the scenario and writes a JSON record: machine (CPU, cores,
  memory, OS), load average before and after, commit and whether the tree was dirty,
  thresholds passed, requests, rate, failures, dropped iterations, latency percentiles and
  the raw k6 metrics.
- Full runs go to `perf/results/` and are committed, pass or fail. A miss is evidence; it
  is never rerun until green. Smoke runs are written to `target/perf/` and are never
  presented as capacity; every one so far, failures and reruns included, is committed in
  `perf/results/smoke/` with an annotated index (`perf/results/smoke/README.md`).
- Run full runs only on a quiet machine (`uptime` load average below about 6 on the 12-core
  development Mac). Same-machine numbers are development observations, not capacity.

| Scenario | Milestone | What it loads | Full-run status |
|---|---|---|---|
| `m0-probes` | M0 | liveness (no I/O) and readiness (one PostgreSQL round trip), 4:1 | owed: machine busy at M0 (load average 462 during the smoke) |
| `m1-auth` | M1 | authenticated project reads: key hash + lookup per request, two projects | owed: machine busy at M1 (load average 130–256 during the smoke) |
| `m2-lifecycle` | M2 | two projects; authorize, capture 600, refund 200 at 100 purchases/s with 5× bursts; then reconciliation | owed; the busy-machine smoke (load 33–43) was correct (0 failed, reconciliation clean) but missed its latency thresholds |
| `m3-uncertain` | M3 | 80% approvals, 10% lost replies, 10% network unavailable, the reconciler running; then nothing open, clean two-way reconciliation | owed; see ROADMAP M3 for the smoke |
| `m4-webhooks` | M4 | purchases with webhooks on: the deliverer signs and delivers every event to `fixtures/dev-store`, which must fulfill every captured payment exactly once | owed; the busy-machine smoke passed its correctness gate and missed p95 by 20 ms |
| `m5-explain` | M5 | seeded purchases, then balances and explain pages at a fixed rate; every account walked to its end must sum to its balance | owed; the busy-machine smoke passed |
| `m6-checkout` | M6 | the hosted checkout as payers use it: a session (API), the page (checkout origin), a read, a paid attempt (authorize and capture) and a repeat of it under the same key; then every session paid once and reconciliation clean | owed: written and parsed (`k6 inspect`); not yet run, smoke or full: the machine was reserved for another million-scale run |
| `m2-refund-race` | M2 | the hot-account and hot-payment refund races (D-027): exact admitted count, then reconciliation | owed; the busy-machine smoke passed (630 admitted exactly, refund p99 39 ms) |

## The web apps (M6)

`scripts/web-perf.sh` records the checkout's and the dashboard's bundle sizes
(`scripts/web-budget.mjs`, gzip; the budgets of D-064 are checked by `scripts/check.sh` and
CI) and a page-load measurement: Chromium headless on loopback, 25 first visits (a fresh
browser context each) and 25 repeat visits per site, until the payer can act (the checkout's
Pay button, the dashboard's Sign in). Results go to `perf/results/<stamp>-web-m6.json` with
the machine, its load and the commit; the browser, the servers and the database share one
machine, so they are development observations, not capacity.
