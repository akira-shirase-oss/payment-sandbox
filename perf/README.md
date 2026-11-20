# Load evidence

**SIMULATED MONEY ONLY.** Benchmark traffic never leaves the machine: the server, the
database and (from M2) the simulated network all listen on loopback.

- `perf/scenarios/<name>.js` — k6 scenarios at a fixed arrival rate (offered load, not
  closed-loop concurrency). `MODE=full` is the workspace standard, ≥1,000,000 operations;
  `MODE=smoke` is a short run for CI and busy machines.
- `scripts/load.sh <name>` builds the release server, runs it on 127.0.0.1:56090 against
  `PS_DATABASE_URL`, runs the scenario and writes a JSON record: machine (CPU, cores,
  memory, OS), load average before and after, commit and whether the tree was dirty,
  thresholds passed, requests, rate, failures, dropped iterations, latency percentiles, the
  raw k6 metrics and `network_stats` (the simulated network's own view: durable-commit and
  queue-wait counters plus its listener's, D-093).
- Full runs go to `perf/results/` and are committed, pass or fail. A miss is evidence; it
  is never rerun until green. Smoke runs are written to `target/perf/` and are never
  presented as capacity; every one so far, failures and reruns included, is committed in
  `perf/results/smoke/` with an annotated index (`perf/results/smoke/README.md`).
- Full runs go to GitHub-hosted runners (D-089): `.github/workflows/load-full.yml`, started by
  hand (`gh workflow run load-full.yml -f scenario=all`), runs each scenario on its own
  runner with its own PostgreSQL and uploads the labelled result; the results are committed
  here as they came out. The development Mac is shared, so a local full run only counts on
  a quiet machine (`uptime` load average below about 6). Every number is a same-machine
  observation (the load generator beside the server), not capacity.
- **Capacity is measured, not assumed (D-102).** A run offered above the runner's knee
  measures only shedding — its p99 is the client's deadline. `scripts/load-capacity.sh
  <scenario> [start_rps]` steps the offered rate through short `MODE=full` runs (the
  scenario's `RATE`/`SECONDS`/`SEED`/`ROUNDS`/`READ_RATE` envs) until its own thresholds
  fail, and writes `<stamp>-<scenario>-capacity.json`: every step, the machine class, the
  knee and `recommended_full_rate` at 60% of it. Evidence full runs go at the recommended
  rate with the limits unchanged; `m8-overload.js` is the explicitly-labelled run where
  shedding IS the expectation (named 503s + `Retry-After`, fast, zero 500s).

| Scenario | Milestone | What it loads | Full-run status |
|---|---|---|---|
| `m0-probes` | M0 | liveness (no I/O) and readiness (one PostgreSQL round trip), 4:1 | **full run on a GitHub-hosted runner: pass** (999,988 requests at 10,000/s, p99 2.4 ms; below) |
| `m1-auth` | M1 | authenticated project reads: key hash + lookup per request, two projects | **full run on a GitHub-hosted runner: miss** (3,734 of 5,000 requests/s served, p95 155 ms; below) |
| `m2-lifecycle` | M2 | two projects; authorize, capture 600, refund 200 at 100 purchases/s with 5× bursts; then reconciliation | **full run on a GitHub-hosted runner: miss** (9.5% failed under the bursts, p99 13.8 s; reconciliation clean; below) |
| `m3-uncertain` | M3 | 80% approvals, 10% lost replies, 10% network unavailable, the reconciler running; then nothing open, clean two-way reconciliation | **full runs on GitHub-hosted runners: miss** (re-run: reconciliation gate clean, 3.35% of load checks failed; below) |
| `m4-webhooks` | M4 | purchases with webhooks on: the deliverer signs and delivers every event to `fixtures/dev-store`, which must fulfill every captured payment exactly once | **full run on a GitHub-hosted runner: pass** (1,000,093 requests, every captured payment fulfilled once; below) |
| `m5-explain` | M5 | seeded purchases, then balances and explain pages at a fixed rate; every account walked to its end must sum to its balance | **full run on a GitHub-hosted runner: miss** (298 reads/s of 2,500 offered, 60% failed; the explain gate held; below) |
| `m6-checkout` | M6 | the hosted checkout as payers use it: a session (API), the page (checkout origin), a read, a paid attempt (authorize and capture) and a repeat of it under the same key; then every session paid once and reconciliation clean | **full run on a GitHub-hosted runner: pass on the re-run** (the first run's latency miss was the harness; below) |
| `m7-developer` | M7 | test stores as the SDK and the CLI drive them: an authorization, its retry (replayed), a conflicting reuse of its key (refused), a capture, a partial refund, a refund past the capacity (refused), a read; one in ten declined; one in five on a third project whose dataset is reset every 30 s under that traffic; then every project's current epoch reconciles with nothing open | **full run on a GitHub-hosted runner: pass** (1,008,135 requests, p99 21 ms; below) |
| `m2-refund-race` | M2 | the hot-account and hot-payment refund races (D-027): exact admitted count, then reconciliation | **run at its defined size on a GitHub-hosted runner: pass** (36,000 offered refunds, below the 1,000,000 rule by design; below) |

## Full runs on GitHub-hosted runners (D-089)

One manual run of `.github/workflows/load-full.yml` (workflow run 44146585972) at commit
`1aca349` on 2026-11-17: each scenario on its own 4-vCPU, 15 GB runner, the k6 load
generator, the release server, the network and PostgreSQL 18 on the same VM. Every result is
committed as it came out, pass or miss; none was rerun. These are observations on shared
cloud hardware with the load generator beside the server, not capacity; "failed" is k6's
`http_req_failed` (any status outside a step's expected ones).

| Scenario | Result | Thresholds | Requests | Requests/s | Failed | Dropped iterations | p95 / p99 | Lock waits > 50 ms | Runner |
|---|---|---|---|---|---|---|---|---|---|
| `m0-probes` | `perf/results/20261117T182128Z-m0-probes-full.json` | **pass** | 999988 | 10000 | 0% | 13 | 0.7 ms / 2.4 ms | 0 | 4 vCPU AMD EPYC 9V45 96-Core Processor, 15 GB |
| `m6-checkout` | `perf/results/20261117T182141Z-m6-checkout-full.json` | **miss** | 999061 | 498 | 0% | 189 | 114.8 ms / 422.6 ms | 0 | 4 vCPU AMD EPYC 9V74 80-Core Processor, 15 GB |
| `m2-lifecycle` | `perf/results/20261117T182142Z-m2-lifecycle-full.json` | **miss** | 1063905 | 350 | 9.54% | 10312 | 532 ms / 13824.9 ms | 0 | 4 vCPU Intel(R) Xeon(R) 6975P-C, 15 GB |
| `m2-refund-race` | `perf/results/20261117T182143Z-m2-refund-race-full.json` | **pass** | 37206 | 588 | 64.43% | 0 | 4.3 ms / 8 ms | 0 | 4 vCPU Intel(R) Xeon(R) 6975P-C, 15 GB |
| `m4-webhooks` | `perf/results/20261117T182144Z-m4-webhooks-full.json` | **pass** | 1000093 | 441 | 0% | 81 | 18.5 ms / 49.5 ms | 13 | 4 vCPU Intel(R) Xeon(R) 6975P-C, 15 GB |
| `m3-uncertain` | `perf/results/20261117T182149Z-m3-uncertain-full.json` | **miss** | 1836007 | 513 | 0% | 0 | 2002.8 ms / 2005.8 ms | 0 | 4 vCPU Intel(R) Xeon(R) 6975P-C, 15 GB |
| `m1-auth` | `perf/results/20261117T182157Z-m1-auth-full.json` | **miss** | 798408 | 3734 | 0.41% | 201596 | 154.6 ms / 174 ms | 0 | 4 vCPU AMD EPYC 7763 64-Core Processor, 15 GB |
| `m5-explain` | `perf/results/20261117T182158Z-m5-explain-full.json` | **miss** | 268985 | 298 | 59.84% | 831222 | 1640.4 ms / 2636.6 ms | 0 | 4 vCPU AMD EPYC 7763 64-Core Processor, 15 GB |
| `m7-developer` | `perf/results/20261117T182205Z-m7-developer-full.json` | **pass** | 1008135 | 558 | 0% | 0 | 9.4 ms / 21.1 ms | 0 | 4 vCPU AMD EPYC 7763 64-Core Processor, 15 GB |

Two passes need a word: `m0-probes` was offered 1,000,000 requests and served 999,988 (13
iterations dropped, within its threshold), and `m2-refund-race`'s 64% "failed" are the
refusals the race exists to produce (`409 insufficient_capacity` on every refund past a
payment's capacity), which its checks count exactly.

What each miss says (from its result file):

- `m1-auth`: the runner served 3,734 authenticated reads/s of the 5,000 offered (201,596
  iterations dropped, 0.41% failed, p95 155 ms); every request hashes and looks up its key,
  and k6 shares the 4 vCPUs. Capacity per core, not correctness.
- `m2-lifecycle`: with the 5× bursts the payment path fell behind on this runner (9.5% of
  load-phase requests failed; p95 of authorize 1.75 s, of capture and refund about 0.51 s,
  against 100 ms; 10,312 iterations dropped); the reconciliation after the run was clean
  (its checks all passed).
- `m3-uncertain`: every load threshold held (the approve path's p95/p99, honest `202
  uncertain` answers; the overall p95 of 2.0 s is the network deadline of the lost and
  unavailable replies), but `teardown()`, which waits for the reconciler to resolve every
  open operation of both projects (up to 150 s each), hit its own 180 s limit (k6 exit 101),
  so the "nothing open, clean two-way reconciliation" gate was not measured. k6 also warned
  of 6.4 M metric series (per-request tags), which costs the generator memory. Owed: a
  teardown limit above its own wait (300 s) and fewer tag values, then a new full run.
- `m5-explain`: the 2,500 reads/s of balances and explanation pages over 100,002 seeded
  postings were far beyond this runner (298 reads/s served, 60% failed, 831,222 iterations
  dropped, p95 1.6 s); the end-of-run gate (every account walked to its end sums to its
  balance) passed.
- `m6-checkout`: every check, the paid-once gate and the reconciliation passed with 0%
  failed; the session, pay and repeat steps missed their latency thresholds (overall p95
  115 ms, p99 423 ms).

### The misses, classified (D-090, D-092)

The first run's five misses were re-run once (workflow run 44161455474, commit `29949da`,
with `load.sh` now recording where the CPU went and the two harness errors fixed). Both
results of each are kept.

| Scenario | Result | Thresholds | Requests | Requests/s | Failed | Dropped iterations | p95 / p99 | Lock waits > 50 ms | Runner |
|---|---|---|---|---|---|---|---|---|---|
| `m5-explain` | `perf/results/20261117T194338Z-m5-explain-full.json` | **miss** | 265209 | 332 | 55.74% | 834999 | 2021.5 ms / 2952.9 ms | 0 | 4 vCPU AMD EPYC 9V45 96-Core Processor, 15 GB |
| `m1-auth` | `perf/results/20261117T194342Z-m1-auth-full.json` | **miss** | 737889 | 3471 | 0.45% | 262114 | 167.7 ms / 186.5 ms | 0 | 4 vCPU AMD EPYC 9V74 80-Core Processor, 15 GB |
| `m6-checkout` | `perf/results/20261117T194342Z-m6-checkout-full.json` | **pass** | 999861 | 498 | 0% | 30 | 24.3 ms / 103.9 ms | 0 | 4 vCPU AMD EPYC 9V74 80-Core Processor, 15 GB |
| `m2-lifecycle` | `perf/results/20261117T194350Z-m2-lifecycle-full.json` | **miss** | 885455 | 291 | 12.17% | 65842 | 1324.9 ms / 48780.1 ms | 0 | 4 vCPU AMD EPYC 9V74 80-Core Processor, 15 GB |
| `m3-uncertain` | `perf/results/20261117T194359Z-m3-uncertain-full.json` | **miss** | 1786803 | 513 | 3.35% | 929 | 2002.9 ms / 2008.1 ms | 0 | 4 vCPU Intel(R) Xeon(R) 6975P-C, 15 GB |

| Scenario | Class | Evidence | Cause and next step |
|---|---|---|---|
| `m6-checkout` | (a) harness, fixed | First run: latency miss only (p95 115 ms, p99 423 ms) with k6 keeping a metric series per session URL. Re-run with ids folded into `name` tags: **pass**, p95 24 ms, p99 104 ms, 0% failed, CPU 73% busy, never saturated | The generator's own cost; nothing owed |
| `m3-uncertain` | (a) harness, fixed; then (b) product | First run: `teardown()` timed out at 180 s before its gate. Re-run: the gate ran and held (both projects: nothing open, journal, payments and network all agree); approve p95 20 ms; but 3.35% of load-phase checks failed with the CPU 64% busy and saturated in 1% of samples | Not the runner. The failed checks are answers that were not the expected `201`/`202` under 300 purchases/s. Done since: the network's own wait telemetry (`network_stats` in every new result; D-093) and the bounded accept wait that turns silent backlog queueing into a fast honest `503` (D-094). Owed: a re-run; if the network's write path shows up in `network_stats`, that is profiled evidence for the next fix |
| `m2-lifecycle` | (b) product | Both runs missed (re-run: 12.2% failed, authorize p99 59 s ≈ k6's own 60 s deadline, `http_req_waiting` p99 48.8 s vs `http_req_blocked` p99 4 s, 65,842 iterations dropped) while the CPU was 40% busy on average and saturated in under 1% of samples; no lock waits over 50 ms | Requests were sent and then sat unanswered — the shape of connections queued in the kernel's listen backlog behind the connection cap, which nothing bounded (D-094 fixed exactly that: an over-cap connection now waits at most `accept_wait` for a serving place, at most `max_pending` of them, then is refused `503` + `Retry-After`). The network's durable `redb` commit remains a candidate for the rest; `network_stats` (D-093) will say. Owed: a re-run |
| `m5-explain` | (b) product | Re-run: 332 reads/s of 2,500, 56% failed; PostgreSQL used 1,570 CPU-seconds (about 2 of 4 cores) against the server's 130; the CPU was saturated in half the samples (the read phase) | `/v1/balances` sums every journal line of the epoch on each call and each explanation page counts and sums the account's lines: a full scan per read over 100,002 postings. Since: derived per-account balances now maintained in the posting statement itself and reconciled to the journal (D-096, the journal stays the authority); the re-run is owed on CI |
| `m1-auth` | (b) runner | Both runs: about 3,500 of 5,000 reads/s. Re-run: the CPU was at least 95% busy in 88% of samples; the server used 172 CPU-seconds and PostgreSQL 199 over 213 s, and k6 about the rest (roughly half the machine). Product cost measured: about 0.23 ms of server CPU and 0.27 ms of PostgreSQL CPU per authenticated read | The rate is not sized for 4 vCPUs shared with the generator; the 25 ms p95 budget was chosen, not measured. Owed: a run with the generator on its own machine (or a larger runner) before the budget or the rate changes |

## The web apps (M6)

`scripts/web-perf.sh` records the checkout's and the dashboard's bundle sizes
(`scripts/web-budget.mjs`, gzip; the budgets of D-064 are checked by `scripts/check.sh` and
CI) and a page-load measurement: Chromium headless on loopback, 25 first visits (a fresh
browser context each) and 25 repeat visits per site, until the payer can act (the checkout's
Pay button, the dashboard's Sign in). Results go to `perf/results/<stamp>-web-m6.json` with
the machine, its load and the commit; the browser, the servers and the database share one
machine, so they are development observations, not capacity.
