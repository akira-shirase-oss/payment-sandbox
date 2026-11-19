#!/usr/bin/env bash
# The gate: what CI runs, run before every commit. Toolchain pin, formatting, Clippy with
# warnings denied, the architecture rule, every test (the database tests against the local
# stack, which this starts if PS_TEST_DATABASE_URL is unset), the web apps (type checks,
# component tests, the browser journeys in Chromium, the size budgets), then cargo-deny if
# installed.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/check-toolchain.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# The crash points exist only in fault-injection builds; lint that build too.
cargo clippy -p sandbox-server -p sandbox-sim --features fault-injection --all-targets --locked -- -D warnings
scripts/check-architecture.sh
# The check must be able to fail: banning a crate the domain does use must trip it.
if ARCH_BANNED="sha2" scripts/check-architecture.sh >/dev/null 2>&1; then
  echo "error: the architecture check did not catch a banned dependency" >&2
  exit 1
fi

if [ -z "${PS_TEST_DATABASE_URL:-}" ]; then
  scripts/stack.sh up >/dev/null
  eval "$(scripts/stack.sh env)"
fi
export PS_TEST_DATABASE_URL
cargo test --workspace --locked
# Drill (a) and the both-processes restart: the real server binary, killed at fault points.
cargo test -p sandbox-server --features fault-injection --test drills --locked
# The network's own crash drill (D-095): aborted between batching and its durable commit,
# it must lose no acknowledged command and record no unacknowledged one.
cargo test -p sandbox-sim --features fault-injection --test drills --locked

# The checkout, the dashboard and the SDK (M6, M7): strict type checks, component and unit
# tests, then the journeys in Chromium against the real processes (which build both apps),
# the examples and the SDK's live tests, then the apps' size budgets (D-064). The web-vitals
# budgets (LCP ≤ 2.5 s, CLS ≤ 0.1, INP ≤ 200 ms on every main page of both apps' production
# builds, D-088) are a journey: e2e/tests/vitals.spec.ts fails the gate on a miss.
# Always a clean install from the lockfile, as CI does: a lockfile out of step with a
# package.json fails here, not first in CI.
npm ci --no-audit --no-fund --prefer-offline
npm run check --workspaces --if-present
npm test --workspaces --if-present
scripts/e2e.sh
node scripts/web-budget.mjs
# Every example a document shows is the file the journeys run (M7).
node scripts/check-doc-examples.mjs

# cargo installs tools into ~/.cargo/bin, which may be missing from PATH (after rustup's).
[ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin"
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny --locked check
else
  echo "note: cargo-deny not installed; supply-chain check skipped locally (CI runs it)"
fi
echo "check ok"
