#!/usr/bin/env bash
# sandbox-domain holds the accounting rules and must stay free of I/O (AGENTS.md
# non-negotiable 9), so every rule is a cheap, deterministic test. This fails if a banned
# crate appears anywhere in its dependency tree, transitively included.
#
# Usage: scripts/check-architecture.sh [crate]     (default: sandbox-domain)
# ARCH_BANNED="a b" overrides the list; used to prove the check can fail.
set -euo pipefail
cd "$(dirname "$0")/.."

crate="${1:-sandbox-domain}"
banned="${ARCH_BANNED:-tokio axum hyper tower tower-http sqlx sqlx-core reqwest mio socket2 libsqlite3-sys rusqlite}"

tree=$(cargo tree --locked -p "$crate" -e normal,build --prefix none --format '{p}' | awk '{print $1}' | sort -u)

found=()
for name in $banned; do
  if grep -qx -- "$name" <<<"$tree"; then
    found+=("$name")
  fi
done

if (( ${#found[@]} )); then
  echo "error: $crate must not depend on: ${found[*]}" >&2
  exit 1
fi
echo "architecture ok: $crate has none of [$banned] in its dependency tree ($(wc -l <<<"$tree" | tr -d ' ') crates)"
