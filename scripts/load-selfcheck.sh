#!/usr/bin/env bash
# A smoke that ASSERTS the classification pipeline end to end (D-097/D-099): run
# m2-lifecycle briefly, then require the result JSON to carry real per-op status counts —
# not an empty aggregate. Usage: eval "$(scripts/stack.sh env)" && scripts/load-selfcheck.sh
set -euo pipefail
cd "$(dirname "$0")/.."

: "${PS_DATABASE_URL:?export PS_DATABASE_URL (scripts/stack.sh env)}"
export PS_LOAD_PORT="${PS_LOAD_PORT:-56090}"

before="$(ls -t target/perf/*m2-lifecycle-smoke.json 2>/dev/null | head -1 || true)"
MODE=smoke bash scripts/load.sh m2-lifecycle >/dev/null 2>&1 || true
latest="$(ls -t target/perf/*m2-lifecycle-smoke.json | head -1)"
[ "$latest" != "$before" ] || { echo "selfcheck: no new smoke result written" >&2; exit 1; }

# The assertion the coordinator asked for: the JSON must contain op_status sub-metrics
# with nonzero counts — the failed runs showed only the aggregate until now.
jq -e '
  ([.k6_summary | to_entries[] | select(.key | startswith("op_status{op:authorize,status:201}") and .value.count > 0)] | length > 0)
  and ([.k6_summary | to_entries[] | select(.key | startswith("op_status{")) | .value.count] | add) > 0
' "$latest" >/dev/null \
  && echo "selfcheck: $latest carries per-op status counts" \
  || { echo "selfcheck: op_status per-op counts missing or empty in $latest" >&2; exit 1; }
