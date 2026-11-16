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
  is never rerun until green. Smoke runs go to `target/perf/` and are never presented as
  capacity.
- Run full runs only on a quiet machine (`uptime` load average below about 6 on the 12-core
  development Mac). Same-machine numbers are development observations, not capacity.

| Scenario | Milestone | What it loads | Full-run status |
|---|---|---|---|
| `m0-probes` | M0 | liveness (no I/O) and readiness (one PostgreSQL round trip), 4:1 | owed: machine busy at M0 (load average 462 during the smoke) |
