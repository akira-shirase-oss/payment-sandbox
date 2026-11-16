# Payment Sandbox — agent instructions

A simulated-money payment platform for development and CI: Rust + Axum +
Tokio + SQLx over PostgreSQL, a separate-process network simulator with its
own store, SvelteKit test checkout and dashboard, a Rust CLI, a TypeScript
SDK and signed webhooks. Contract: the workspace engineering guide
`docs/Project_Guides/05_Payment_Sandbox.docx` (kept outside this
repository) — extract with `textutil -convert txt -stdout`. When these documents and the guide disagree,
the guide wins.

## Read first

1. `docs/END_GOAL.md` — what done means, scope, invariants, the four required
   failure cases, the ship gate.
2. `docs/ARCHITECTURE.md` — sources of truth, request paths, layout.
3. `docs/DECISIONS.md` — seeded decisions plus the open questions; do not
   silently resolve an open question — record a new D-NNN when one is decided.
4. `docs/ROADMAP.md` — M0–M8 order and each milestone's proof gate.

## Non-negotiables

1. **The journal is the only authority.** Posted entries are immutable and
   balanced per currency; corrections happen only as reversing entries.
   Materialized balances are derived and reconciled to the journal — never
   authoritative on their own.
2. **No transaction spans the simulator.** The operation identity commits
   before the network call; a lost response stays `uncertain` until readback,
   and reconciliation uses that same identity. `uncertain` is a first-class
   reported state, not an error to hide.
3. **Capacity is reserved transactionally.** Capture cannot exceed the
   authorized remainder and refund cannot exceed the captured remainder;
   simultaneous requests cannot overrun — the unauthorized excess fails.
4. **Idempotency is strict.** Business operation keys are unique; the same
   key with a different payload is a conflict, never a silent
   reinterpretation of an accepted operation.
5. **At-least-once, honestly.** Inbox/outbox ids deduplicate logical work but
   cannot eliminate repeated external deliveries; fulfillment carries its own
   business key; never claim exactly-once delivery.
6. **Sandbox-only by construction.** Synthetic tokens, sandbox-scoped keys,
   prominent labels in binaries/UI/README/artifacts; raw card fields are
   rejected; no production provider routes exist; outbound calls are
   allowlisted (loopback only in the local/CI mode, D-023); projects are
   isolated by composite keys (D-013); API keys are hashed and webhook
   signing secrets are encrypted at rest (D-020); dataset reset requires
   typed confirmation and opens a new epoch (D-022). No UI has a
   card-number field (D-019). No real payment credential ever enters
   the repo, a fixture or a test.
7. **Virtual time cannot rewrite history.** The simulator's test clocks drive
   scenarios; already-posted journal history never changes under a clock
   jump, crash or restore.
8. **Bounded everything + evidence over claims.** The workspace rules apply
   in full: every queue, body, retry loop and pool has a limit and a
   deadline; load results carry machine + commit; misses are kept, never
   rerun until green; laptop numbers are development observations; benchmark
   traffic never touches a real payment network.
9. **Pure domain.** `sandbox-domain` carries no I/O — enforced like the other
   projects' domain crates.

## Rust rules

- Use the `rust-mastery` skill, and prefix every cargo command with
  `export PATH="$(brew --prefix rustup)/bin:$PATH"` — Homebrew's cargo comes
  first on `PATH` and ignores the pinned toolchain.
- Typed errors (`thiserror`) in libraries; `anyhow` only in binaries. No
  `unwrap`/`expect` on external input. Every I/O await has a deadline. No
  secrets, tokens or payment payloads in logs.
- Money is checked integer arithmetic with an explicit currency scale —
  never floats, never unchecked `+`/`-` on amounts.

## Frontend rules

- SvelteKit + TypeScript (strict) + Tailwind CSS v4 + shadcn-svelte
  (D-035), starting from this repository's own copy of the Colophon UI
  package in `packages/ui` — never a dependency on the Colophon repository.
  Checkout and dashboard are static
  client-side apps served by the Rust server on **separate origins**
  (separate listeners, strict CSP on checkout — D-029); they never query
  PostgreSQL or hold payment authority. Everything user-visible is
  labeled as simulated.

## Testing rule (shared workspace standard)

Every feature ships regression tests + a fixture path + a load scenario in
the same branch; every fixed bug gets a test that fails without the fix.
The four contract failure cases are drill obligations, not options.
Reproduce lost responses and concurrent partial refunds before touching
subscriptions (the guide's sequencing is deliberate).

## Verification

`cargo fmt --all --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked` — before every commit, plus the
crash/reconciliation drills when ledger, simulator or delivery code changed.
Full load runs are ≥1,000,000 operations per the workspace standard; results
are committed under `perf/results/` with machine + commit.

## Working rules

- Small slices: each states the user outcome, the data invariant, the
  failure case, the permission and the acceptance test.
- Report exactly what ran and what passed; honest `unknown`/`uncertain`
  beats a reassuring lie.
- Repository: `github.com/akira-shirase-oss/payment-sandbox`. Work lands on
  `main` in small green steps: `scripts/check.sh` passes, then commit and
  push. No long-lived branches, no force-push, no history rewrites.
- A roadmap status cell says Done only when it names its tests and
  evidence path (D-031).
