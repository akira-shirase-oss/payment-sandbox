#!/usr/bin/env bash
# Runs one load scenario against a release build of the server on the local stack and
# keeps the evidence (workspace load-test standard).
#
#   MODE=full  scripts/load.sh <scenario>   the full run (≥1,000,000 operations); the
#                                           result is written to perf/results/ to be committed
#   MODE=smoke scripts/load.sh <scenario>   a short run; written under target/perf/ only,
#                                           never committed as capacity
#
# <scenario> is perf/scenarios/<scenario>.js. Every result records the machine, the OS, the
# commit (and whether the tree was dirty), and the load average before and after, so a run
# on a busy machine is visibly a busy-machine observation. A threshold miss fails the run and
# is kept as evidence; it is never rerun until green.
#
# Needs: k6 (scripts/install-k6.sh), jq, and PS_DATABASE_URL (scripts/stack.sh env).
set -euo pipefail
cd "$(dirname "$0")/.."

scenario="${1:?usage: scripts/load.sh <scenario>}"
mode="${MODE:-smoke}"
script="perf/scenarios/${scenario}.js"
[ -f "$script" ] || { echo "error: no $script" >&2; exit 2; }
: "${PS_DATABASE_URL:?export PS_DATABASE_URL (scripts/stack.sh env)}"
port="${PS_LOAD_PORT:-56090}"

cargo build --release --locked -p sandbox-server --bin sandbox-server >/dev/null
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [ "$mode" = "full" ]; then outdir="perf/results"; else outdir="target/perf"; fi
mkdir -p "$outdir"
summary="$(mktemp)"
log="target/perf/server-${scenario}-${stamp}.log"
mkdir -p target/perf

load_before="$(uptime | sed 's/.*load averages*: *//')"
# A fresh operator token per run: scenarios create their own projects with it.
bootstrap="load-$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
PS_BOOTSTRAP_TOKEN="$bootstrap" PS_LISTEN="127.0.0.1:${port}" PS_LOG=warn PS_LOG_FORMAT=json \
  ./target/release/sandbox-server >"$log" 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true; rm -f "$summary"' EXIT
for _ in $(seq 1 100); do
  curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null

set +e
BOOTSTRAP="$bootstrap" BASE_URL="http://127.0.0.1:${port}" MODE="$mode" k6 run --quiet \
  --summary-export "$summary" "$script"
k6_status=$?
set -e
load_after="$(uptime | sed 's/.*load averages*: *//')"
rss_kb="$(ps -o rss= -p "$server" | tr -d ' ' || echo 0)"

if [ "$(uname -s)" = "Darwin" ]; then
  cpu="$(sysctl -n machdep.cpu.brand_string)"; cores="$(sysctl -n hw.ncpu)"
  mem_gb="$(( $(sysctl -n hw.memsize) / 1073741824 ))"; os="macOS $(sw_vers -productVersion)"
else
  cpu="$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"; cores="$(nproc)"
  mem_gb="$(( $(grep MemTotal /proc/meminfo | awk '{print $2}') / 1048576 ))"; os="$(uname -sr)"
fi
commit="$(git rev-parse HEAD)"
dirty=false; git diff --quiet HEAD -- . ':!perf/results' || dirty=true

out="${outdir}/${stamp}-${scenario}-${mode}.json"
jq -n \
  --arg scenario "$scenario" --arg mode "$mode" --arg stamp "$stamp" \
  --arg commit "$commit" --argjson dirty "$dirty" \
  --arg cpu "$cpu" --arg cores "$cores" --arg mem_gb "$mem_gb" --arg os "$os" \
  --arg load_before "$load_before" --arg load_after "$load_after" \
  --arg rss_kb "$rss_kb" --argjson k6_status "$k6_status" \
  --slurpfile summary "$summary" '
  {
    scenario: $scenario, mode: $mode, started_utc: $stamp,
    commit: $commit, tree_dirty: $dirty,
    machine: {cpu: $cpu, cores: ($cores|tonumber), memory_gb: ($mem_gb|tonumber), os: $os,
              load_average_before: $load_before, load_average_after: $load_after,
              note: "same-machine load generator: a development observation, not capacity"},
    server_rss_kb_at_end: ($rss_kb|tonumber),
    thresholds_passed: ($k6_status == 0),
    k6_exit_status: $k6_status,
    requests: $summary[0].metrics.http_reqs.count,
    requests_per_second: $summary[0].metrics.http_reqs.rate,
    failed_rate: $summary[0].metrics.http_req_failed.value,
    dropped_iterations: ($summary[0].metrics.dropped_iterations.count // 0),
    latency_ms: ($summary[0].metrics.http_req_duration | {min, med, avg, "p(90)", "p(95)", "p(99)", max}),
    thresholds: [$summary[0].metrics | to_entries[] | select(.value.thresholds) |
                 {metric: .key, thresholds: .value.thresholds}],
    k6_summary: $summary[0].metrics
  }' >"$out"
echo "result: $out"
jq '{scenario, mode, commit, thresholds_passed, requests, requests_per_second, failed_rate, dropped_iterations, latency_ms, machine: {cpu: .machine.cpu, load_average_before: .machine.load_average_before}}' "$out"
exit "$k6_status"
