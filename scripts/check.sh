#!/usr/bin/env bash
# The gate: what CI runs, run before every commit. Toolchain pin, formatting, Clippy with
# warnings denied, the architecture rule, every test (the database tests against the local
# stack, which this starts if PS_TEST_DATABASE_URL is unset), then cargo-deny if installed.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/check-toolchain.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
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

# cargo installs tools into ~/.cargo/bin, which may be missing from PATH (after rustup's).
[ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin"
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny --locked check
else
  echo "note: cargo-deny not installed; supply-chain check skipped locally (CI runs it)"
fi
echo "check ok"
