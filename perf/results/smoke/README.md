# Smoke runs (development observations, never capacity)

Every smoke run so far, failures included, copied from `target/perf/`. All ran on the
busy development Mac (Apple M2 Pro, 12 cores, 32 GB); the load average before each run is in
its record. They ran on working trees with uncommitted changes on top of the commit shown
(`+dirty`; until the evidence fix the dirty check ignored untracked files, so the first
M0 record says clean although its code was not yet committed). None is a full run: the full ≥1,000,000-request runs are owed
(`perf/README.md`).

| File | Scenario | Commit (+dirty) | Requests | Thresholds | Load before | Note |
|---|---|---|---|---|---|---|
| `20261116T214805Z-m0-probes-smoke.json` | m0-probes | 530e7ae | 20001 | passed | 462.49 169.43 87.07 | M0 smoke. (The dirty check then missed untracked files: the M0 code was uncommitted.) |
| `20261116T215639Z-m1-auth-smoke.json` | m1-auth | bbb8104 +dirty | 10005 | passed | 130.82 256.93 184.58 | M1 smoke. |
| `20261116T221528Z-m2-lifecycle-smoke.json` | m2-lifecycle | d6228f5 +dirty | 3064 | **failed** | 57.69 59.98 91.96 | Latency missed: about 15 database round trips per operation; cut before the next run. |
| `20261116T221905Z-m2-lifecycle-smoke.json` | m2-lifecycle | d6228f5 +dirty | 3301 | **failed** | 28.34 41.00 76.33 | Correct, but the teardown report timed out at the 1 s statement deadline; report given its own deadline. |
| `20261116T222048Z-m2-lifecycle-smoke.json` | m2-lifecycle | d6228f5 +dirty | 2872 | **failed** | 54.57 47.09 74.79 | Latency missed at load average 54. |
| `20261116T222126Z-m2-refund-race-smoke.json` | m2-refund-race | d6228f5 +dirty | 1874 | **failed** | 64.25 50.42 74.73 | **Found D-033**: hot-payment waiters starved the pool (1,030 pool timeouts) and 41 reservations stayed held; only 49 of 630 admitted. |
| `20261116T222348Z-m2-refund-race-smoke.json` | m2-refund-race | d6228f5 +dirty | 2066 | **failed** | 54.26 58.02 74.64 | After the D-033 fix; a scenario bug (`__ITER` is per virtual user) aimed refunds at 6 of 21 payments: 180 admitted. Fixed in the scenario. |
| `20261116T222415Z-m2-refund-race-smoke.json` | m2-refund-race | d6228f5 +dirty | 2066 | passed | 45.78 55.50 73.14 | Passed: exactly 630 admitted. |
| `20261116T223541Z-m2-refund-race-smoke.json` | m2-refund-race | d6228f5 +dirty | 2065 | passed | 37.15 44.48 57.72 | Passed on the redb simulator store (D-034). |
| `20261116T223556Z-m2-lifecycle-smoke.json` | m2-lifecycle | d6228f5 +dirty | 3121 | **failed** | 33.10 43.26 57.05 | Correct (0 failed, reconciliation clean); latency missed at load average 33. |
| `20261116T224729Z-m3-uncertain-smoke.json` | m3-uncertain | 8fd1338 +dirty | 1229 | **failed** | 540.19 256.78 135.30 | **Found** the listing size cap bug in reverse reconciliation (fixed, regression test); load average 540. |
| `20261116T225220Z-m3-uncertain-smoke.json` | m3-uncertain | 8fd1338 +dirty | 1091 | passed | 163.22 218.55 157.15 | Passed: 120 uncertain answers, all resolved. |
| `20261116T230750Z-m4-webhooks-smoke.json` | m4-webhooks | 9c939df +dirty | 1007 | **failed** | 24.79 38.32 77.86 | **Found D-037**: the teardown report timed out at the 5 s request deadline. |
| `20261116T231029Z-m4-webhooks-smoke.json` | m4-webhooks | 9c939df +dirty | 1008 | **failed** | 32.90 35.55 70.07 | Correctness gate passed (501 fulfilled exactly once); p95 120 ms missed the 100 ms threshold. |
| `20261116T231503Z-m5-explain-smoke.json` | m5-explain | 195022e +dirty | 4306 | passed | 21.56 29.32 57.84 | Passed. |
| `20261117T012033Z-m0-probes-smoke.json` | m0-probes | a92e1f4 +dirty | 20001 | passed | 41.69 57.54 43.12 | After the role split (D-038): the load runner sets up roles and migrates as the owner. |
