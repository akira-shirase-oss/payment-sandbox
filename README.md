# Payment Sandbox

> **SIMULATED MONEY ONLY.** No real cards, balances or banking connection,
> ever. Every response carries `Sandbox-Simulated: true`.
>
> **Status — M0 (the spine) and M1 (projects and sandbox keys) done; M2
> (payment lifecycle) next.** Milestones, proof gates and the
> evidence behind each status live in `docs/ROADMAP.md`.

## Run it

```sh
export PATH="$(brew --prefix rustup)/bin:$PATH"   # the pinned toolchain (rust-toolchain.toml)
scripts/stack.sh up                                # PostgreSQL on 127.0.0.1:56432 (compose project payment-sandbox)
eval "$(scripts/stack.sh env)"
scripts/check.sh                                   # the gate CI runs: fmt, clippy, architecture, tests, cargo-deny
cargo run -p sandbox-server                        # API on 127.0.0.1:56080
```

Ports (all loopback): 56432 PostgreSQL, 56080 API, 56081 simulator (M2),
56090 load-test server. Load runs: `MODE=smoke|full scripts/load.sh <scenario>`
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
