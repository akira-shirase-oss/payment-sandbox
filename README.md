# Payment Sandbox

> **SIMULATED MONEY ONLY.** No real cards, balances or banking connection,
> ever. Every response carries `Sandbox-Simulated: true`.
>
> **Status — M0 to M7 in progress, version 1.0.0 not yet tagged: their functional gates pass
> (tests, drills, reconciliation, the browser journeys in Chromium, and for M7 the CLI, the
> TypeScript SDK, the scenario library's reproducibility and the release path, with the v1
> readiness review's findings fixed, D-074 to D-089). The full ≥1,000,000-request load runs
> ran once on GitHub-hosted runners (`perf/README.md`): `m0-probes`, `m4-webhooks` and
> `m7-developer` passed, `m2-refund-race` passed at its defined size, and `m1-auth`,
> `m2-lifecycle`, `m3-uncertain`, `m5-explain` and `m6-checkout` missed (kept as they came
> out; the end-of-run reconciliation and explanation gates held wherever they ran, and
> `m3-uncertain`'s timed out before it did). No milestone is Done yet.**
> Milestones, proof gates and the evidence behind each status live in `docs/ROADMAP.md`.

## Run it

From a clone, on macOS or Linux with Docker, rustup and Node 22.17+:

```sh
export PATH="$(brew --prefix rustup)/bin:$PATH"   # macOS with rustup: the pinned toolchain (rust-toolchain.toml)
scripts/stack.sh up && eval "$(scripts/stack.sh env)"  # PostgreSQL on 127.0.0.1:56432; dev URLs and credentials (D-046)
cargo run -p sandbox-server -- setup               # roles ps_owner / ps_runtime / ps_app + the database (D-038)
cargo run -p sandbox-server -- migrate             # migrations, as the owner
npm ci && npm run build                            # the checkout and the dashboard
export PS_BOOTSTRAP_TOKEN=$(openssl rand -hex 24)  # the operator's token: it creates projects
export PS_SECRET_KEY=$(openssl rand -hex 32)       # seals webhook signing secrets at rest
cargo run -p sandbox-sim &                         # the simulated network on 127.0.0.1:56081
PS_CHECKOUT_DIR=apps/checkout/build PS_DASHBOARD_DIR=apps/dashboard/build \
  cargo run -p sandbox-server &                    # API 127.0.0.1:56080, checkout 56083, dashboard 56084
```

Then create a project with the bootstrap token. The answer holds the project's first
admin key, shown once; keep it in your environment, never on a command line:

```sh
curl -s -X POST http://127.0.0.1:56080/v1/projects \
  -H "Authorization: Bearer $PS_BOOTSTRAP_TOKEN" -H 'Content-Type: application/json' \
  -d '{"slug": "my-store", "name": "My store"}'
export PS_API_KEY=sk_sim_...                       # the api_key.secret it printed
cargo run -q -p sandbox-cli -- status
cargo run -q -p sandbox-cli -- payments create --amount 42.00 --currency USD
```

Sign in to the dashboard at `http://dashboard.localhost:56084` with the same key. From
here, `docs/DEVELOPERS.md` takes a store through the CLI, the TypeScript SDK, the hosted
checkout, webhooks and the failure-scenario library; `docs/API.md` lists every endpoint.

Working on the sandbox itself:

```sh
scripts/check.sh                                   # the gate CI runs: fmt, clippy, architecture, tests, web apps and journeys, cargo-deny
scripts/e2e.sh                                     # the browser journeys only (Chromium, ports 56460-56465)
scripts/restore-rehearsal.sh payment_sandbox ps_rehearsal   # pg_dump → fresh database → identical fingerprint (D-053)
cargo run -p sandbox-dev-store                     # the development store (a webhook consumer) on 127.0.0.1:56082
```

A store creates a checkout session (`POST /v1/checkout_sessions`) and sends the payer to
the URL it returns; the payer picks what the simulated network does (there is no card
field) and the page shows what really happened, `uncertain` included. The dashboard signs
in with a project's sandbox key and shows payments, holds, journal entries, every balance
explained line by line, reconciliation, webhook deliveries and (dev profile) the
simulator's controls. Each is its own origin under a strict Content-Security-Policy
(D-060–D-065); the dashboard answers on its own host name, `dashboard.localhost`, so its
session cookie never reaches another port, and its API is for its own page only (scripts
and `curl` use `/v1` with a key).

Ports (all loopback): 56080 API, 56081 simulated network, 56082 development store, 56083
checkout, 56084 dashboard, 56432 PostgreSQL, 56450 the empty cluster of the cross-cluster
restore test, 56090 load-test server. Loopback is not a credential: the simulator answers
only the server's `PS_SIM_TOKEN`, and the test controls (the simulator's faults and clock,
the development store's `PUT /config`) exist only when switched on, by default only with
`PS_PROFILE=dev`, behind their own operator credentials (D-046). Load runs:
`MODE=smoke|full scripts/load.sh <scenario>` (`perf/README.md`).

An independently owned, **simulated-money** payment platform for development
and CI. CMS test stores use our checkout, APIs and signed webhooks without a
Stripe account; a separate simulated network supplies approvals, declines and
uncertain outcomes. We implement internal accounting and recovery on
PostgreSQL — an immutable, double-entry journal that every synthetic balance
must reconcile to. This is not a live processor: no real cards, balances or
banking connection.

## Documents

- `docs/DEVELOPERS.md` — the developer guide: the CLI, the TypeScript SDK
  (`packages/sdk-ts`), the hosted checkout, webhooks, the failure-scenario library, reset;
  its examples run in CI as written
- `docs/API.md` — the API reference: every endpoint, its scope, body and answers, and the
  error codes (a test keeps it identical to the server's routes)
- `docs/OPERATING.md` — the operator guide for a shared host: install (release build,
  `deploy/systemd`, `deploy/env`, `deploy/Caddyfile`), first run, upgrade, backup and
  restore, troubleshooting
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
