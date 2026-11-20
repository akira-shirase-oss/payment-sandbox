#!/usr/bin/env bash
# A smoke that ASSERTS the classification pipeline end to end (D-097/D-099): run
# m2-lifecycle briefly, then require the result JSON to carry real per-op status counts —
# not an empty aggregate. Usage: eval "$(scripts/stack.sh env)" && scripts/load-selfcheck.sh
set -euo pipefail
cd "$(dirname "$0")/.."

: "${PS_DATABASE_URL:?export PS_DATABASE_URL (scripts/stack.sh env)}"
export PS_LOAD_PORT="${PS_LOAD_PORT:-56090}"

before="$(ls -t target/perf/*m2-lifecycle-smoke.json 2>/dev/null | head -1 || true)"
load_log=target/perf/selfcheck-load.log
mkdir -p target/perf
MODE=smoke bash scripts/load.sh m2-lifecycle >"$load_log" 2>&1 || true
latest="$(ls -t target/perf/*m2-lifecycle-smoke.json 2>/dev/null | head -1 || true)"
if [ -z "$latest" ] || [ "$latest" = "$before" ]; then
  echo "selfcheck: no new smoke result written; load.sh said:" >&2
  tail -20 "$load_log" >&2
  exit 1
fi

# The assertion the coordinator asked for: the JSON must contain op_status sub-metrics
# with nonzero counts — the failed runs showed only the aggregate until now.
jq -e '
  ([.k6_summary | to_entries[] | select((.key | startswith("op_status{op:authorize,status:201}")) and (.value.count > 0))] | length > 0)
  and ([.k6_summary | to_entries[] | select(.key | startswith("op_status{")) | .value.count] | add) > 0
' "$latest" >/dev/null \
  && echo "selfcheck: $latest carries per-op status counts" \
  || { echo "selfcheck: op_status per-op counts missing or empty in $latest" >&2; exit 1; }

# The m2 regression (run 44658104817, class (a)): a refund may only follow a captured
# capture — the scenario returns early when the capture is shed, so an honest domain
# 409 refund_exceeds_captured can never be produced by the script itself. A nonzero
# count here means the guard regressed.
jq -e '
  [.k6_summary | to_entries[] | select(.key | startswith("op_status{op:refund,status:409}")) | .value.count] | add // 0 | . == 0
' "$latest" >/dev/null \
  && echo "selfcheck: no refund 409s — refunds are gated on capture" \
  || { echo "selfcheck: refund 409s present — the capture-gate regressed" >&2; exit 1; }
