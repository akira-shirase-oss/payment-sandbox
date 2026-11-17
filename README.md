# Payment Sandbox

> **SIMULATED MONEY ONLY.** No real cards, balances or banking connection,
> ever. Every response carries `Sandbox-Simulated: true`.
>
> **Status — M0 to M6 in progress: their functional gates pass (tests, drills,
> reconciliation, and for M6 the browser journeys in Chromium), and every full
> ≥1,000,000-request load run is still owed; only smoke runs exist, failures included
> (`perf/results/smoke/`), and M6's checkout scenario has not run yet.** Milestones, proof
> gates and the evidence behind each status live in `docs/ROADMAP.md`.

## Run it

```sh
export PATH="$(brew --prefix rustup)/bin:$PATH"   # the pinned toolchain (rust-toolchain.toml)
scripts/stack.sh up                                # PostgreSQL on 127.0.0.1:56432, a second (empty) cluster on 56450 for the cross-cluster restore test
eval "$(scripts/stack.sh env)"                     # database URLs, PS_PROFILE=dev and dev credentials (D-046)
cargo run -p sandbox-server -- setup               # roles ps_owner / ps_runtime / ps_app + database (D-038)
cargo run -p sandbox-server -- migrate             # migrations, as the owner
scripts/check.sh                                   # the gate CI runs: fmt, clippy, architecture, tests, web apps and journeys, cargo-deny
scripts/restore-rehearsal.sh payment_sandbox ps_rehearsal   # pg_dump → fresh database → identical fingerprint (D-053)
cargo run -p sandbox-sim                           # simulated network on 127.0.0.1:56081
cargo run -p sandbox-server                        # API on 127.0.0.1:56080, as ps_app (webhooks need PS_SECRET_KEY)
cargo run -p sandbox-dev-store                     # the development store on 127.0.0.1:56082
npm ci && npm run build                            # the checkout and the dashboard (Node 22.17+)
PS_CHECKOUT_DIR=apps/checkout/build PS_DASHBOARD_DIR=apps/dashboard/build \
  cargo run -p sandbox-server                      # ... with the checkout on :56083, the dashboard on :56084
scripts/e2e.sh                                     # the browser journeys (Chromium, ports 56460-56465)
```

A store creates a checkout session (`POST /v1/checkout_sessions`) and sends the payer to
the URL it returns; the payer picks what the simulated network does (there is no card
field) and the page shows what really happened, `uncertain` included. The dashboard signs
in with a project's sandbox key and shows payments, holds, journal entries, every balance
explained line by line, reconciliation, webhook deliveries and (dev profile) the
simulator's controls. Each is its own origin under a strict Content-Security-Policy
(D-060–D-064).

Ports (all loopback): 56432 PostgreSQL, 56080 API, 56081 simulator (M2),
56090 load-test server. Loopback is not a credential: the simulator answers
only the server's `PS_SIM_TOKEN`, and the test controls (the simulator's
faults and clock, the development store's `PUT /config`) exist only when
switched on, by default only with `PS_PROFILE=dev`, behind their own
operator credentials (D-046). Load runs: `MODE=smoke|full scripts/load.sh <scenario>`
(`perf/README.md`).

An independently owned, **simulated-money** payment platform for development
and CI. CMS test stores use our checkout, APIs and signed webhooks without a
Stripe account; a separate simulated network supplies approvals, declines and
uncertain outcomes. We implement internal accounting and recovery on
PostgreSQL — an immutable, double-entry journal that every synthetic balance
must reconcile to. This is not a live processor: no real cards, balances or
banking connection.

## Documents

- `docs/END_GOAL.md` — what the system is when shipped, scope in/out, the
  non-negotiable invariants, the four required failure cases, the ship gate
- `docs/ARCHITECTURE.md` — sources of truth, authorize/capture/refund/webhook/
  recovery paths, repository layout, the consistency story
- `docs/ROADMAP.md` — M0 the spine → M8 billing direction, each with its
  proof gate
- `docs/DECISIONS.md` — seeded design trade-offs (D-001..) and the open
  questions the guide deliberately leaves us

Contract: the workspace engineering guide
`docs/Project_Guides/05_Payment_Sandbox.docx` (kept outside this
repository) — extract with `textutil -convert txt -stdout <file>`.

## What counts as done

The ship gate: **a development store can test payment failures reproducibly,
and every synthetic balance can be explained from journal entries.** Under
the workspace standard that means executed evidence — the contract's four
failure cases proven, and million-scale load results committed under
`perf/results/` with machine + commit.
