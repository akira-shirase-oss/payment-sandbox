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
# The server's network is a release build of sandbox-sim on the next port, and the
# development store (fixtures/dev-store) on the one after, each with a fresh store per run;
# webhooks are on with a fresh PS_SECRET_KEY. Lock waits over 50 ms (PostgreSQL's log, when the local stack runs
# in Docker) are counted into the record.
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
sim_port="$((port + 1))"

cargo build --release --locked -p sandbox-server -p sandbox-sim -p sandbox-dev-store >/dev/null
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [ "$mode" = "full" ]; then outdir="perf/results"; else outdir="target/perf"; fi
mkdir -p "$outdir"
summary="$(mktemp)"
log="target/perf/server-${scenario}-${stamp}.log"
mkdir -p target/perf

# Roles, database and schema as their owners (D-038); the server then runs as ps_app.
: "${PS_ADMIN_DATABASE_URL:?export PS_ADMIN_DATABASE_URL (scripts/stack.sh env)}"
: "${PS_MIGRATE_DATABASE_URL:?export PS_MIGRATE_DATABASE_URL (scripts/stack.sh env)}"
./target/release/sandbox-server setup >/dev/null
./target/release/sandbox-server migrate >/dev/null
load_before="$(uptime | sed 's/.*load averages*: *//')"
started_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
sim_db="target/perf/sim-${scenario}-${stamp}.redb"
# Fresh credentials per run (D-046): the network's, which the server presents, and the
# store's operator credential for PUT /config. The network's test controls stay off.
sim_token="sim-$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
store_control="store-$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
PS_SIM_TOKEN="$sim_token" PS_SIM_CONTROL=off \
PS_SIM_LISTEN="127.0.0.1:${sim_port}" PS_SIM_DB="$sim_db" PS_SIM_LOG=warn \
  ./target/release/sandbox-sim >"target/perf/sim-${scenario}-${stamp}.log" 2>&1 &
sim=$!
store_port="$((port + 2))"
PS_STORE_CONTROL=on PS_STORE_CONTROL_TOKEN="$store_control" \
PS_STORE_LISTEN="127.0.0.1:${store_port}" PS_STORE_DB="target/perf/store-${scenario}-${stamp}.redb" \
  ./target/release/sandbox-dev-store >"target/perf/store-${scenario}-${stamp}.log" 2>&1 &
store=$!
# The checkout's own listener for the M6 scenarios (D-061), serving a one-page stand-in:
# the load is the server's (sessions, attempts, the page from memory), not the app's.
checkout_env=()
checkout_url=""
if [[ "$scenario" == m6-* ]]; then
  site="target/perf/checkout-site-${stamp}"
  mkdir -p "$site"
  printf '<!doctype html><title>checkout</title><script>start()</script>\n' >"$site/index.html"
  checkout_url="http://127.0.0.1:$((port + 3))"
  checkout_env=(PS_CHECKOUT_DIR="$site" PS_CHECKOUT_LISTEN="127.0.0.1:$((port + 3))")
fi
# A fresh operator token per run: scenarios create their own projects with it.
bootstrap="load-$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
# Load runs measure each route's own cost: the per-project report budget (D-047), which
# would turn a one-project explain scenario into a measurement of the limiter, is lifted
# unless the caller sets it. The limiter itself is tested in tests/reports.rs.
PS_REPORT_BURST="${PS_REPORT_BURST:-1000000000}" \
PS_REPORT_REFILL_PER_MINUTE="${PS_REPORT_REFILL_PER_MINUTE:-1000000000}" \
PS_REPORT_PER_PROJECT="${PS_REPORT_PER_PROJECT:-4096}" PS_REPORT_CONCURRENCY="${PS_REPORT_CONCURRENCY:-256}" \
PS_SIM_URL="http://127.0.0.1:${sim_port}" PS_SIM_TOKEN="$sim_token" PS_RECONCILE_INTERVAL_MS=2000 \
PS_WEBHOOK_PORTS="$((port + 2))" \
PS_SECRET_KEY="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')" \
PS_BOOTSTRAP_TOKEN="$bootstrap" PS_LISTEN="127.0.0.1:${port}" PS_LOG=warn PS_LOG_FORMAT=json \
  env ${checkout_env[@]+"${checkout_env[@]}"} ./target/release/sandbox-server >"$log" 2>&1 &
server=$!
trap 'kill "$server" "$sim" "$store" 2>/dev/null || true; rm -f "$summary"' EXIT
for _ in $(seq 1 100); do
  curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null

# Where the CPU went (Linux: /proc), so a miss can be told apart as the machine's or the
# product's (D-090): the whole machine's busy share every 5 s, and the CPU seconds of the
# server, the network and PostgreSQL's container over the run; the rest is mostly k6.
cpu_samples="$(mktemp)"
proc_cpu() { # utime+stime of a pid, in clock ticks
  [ -r "/proc/$1/stat" ] && awk '{print $14 + $15}' "/proc/$1/stat" || echo 0
}
pg_cpu_usec() {
  docker compose -f deploy/compose.yaml exec -T postgres sh -c 'grep usage_usec /sys/fs/cgroup/cpu.stat' 2>/dev/null \
    | awk '{print $2}' || true
}
sampler=""
if [ -r /proc/stat ]; then
  ( while :; do grep '^cpu ' /proc/stat >>"$cpu_samples"; sleep 5; done ) &
  sampler=$!
  server_ticks0="$(proc_cpu "$server")" sim_ticks0="$(proc_cpu "$sim")" pg_usec0="$(pg_cpu_usec)"
fi
run_started=$SECONDS
set +e
STORE_URL="http://127.0.0.1:${store_port}" STORE_TOKEN="$store_control" CHECKOUT_URL="$checkout_url" \
BOOTSTRAP="$bootstrap" BASE_URL="http://127.0.0.1:${port}" MODE="$mode" k6 run --quiet \
  --summary-export "$summary" "$script"
k6_status=$?
run_seconds=$((SECONDS - run_started))
cpu_use='{"note": "not measured (no /proc on this machine)"}'
if [ -n "$sampler" ]; then
  kill "$sampler" 2>/dev/null || true
  wait "$sampler" 2>/dev/null || true
  hz="$(getconf CLK_TCK)"
  server_s="$(( ($(proc_cpu "$server") - server_ticks0) / hz ))"
  sim_s="$(( ($(proc_cpu "$sim") - sim_ticks0) / hz ))"
  pg_usec1="$(pg_cpu_usec)"
  pg_s=null
  [ -n "$pg_usec0" ] && [ -n "$pg_usec1" ] && pg_s="$(( (pg_usec1 - pg_usec0) / 1000000 ))"
  # Per 5 s interval: busy share of all cores (user+nice+system+irq+softirq+steal over all).
  awk '{ busy = $2 + $3 + $4 + $7 + $8 + $9; total = busy + $5 + $6
         if (NR > 1 && total > ptotal) printf "%.4f\n", (busy - pbusy) / (total - ptotal)
         pbusy = busy; ptotal = total }' "$cpu_samples" | sort -n >"$cpu_samples.busy"
  cpu_use="$(awk -v cores="$(nproc)" -v secs="$run_seconds" -v server="$server_s" -v sim="$sim_s" -v pg="$pg_s" '
    { v[NR] = $1; sum += $1; if ($1 >= 0.95) sat++ }
    END {
      if (NR == 0) { print "{\"note\": \"no samples\"}"; exit }
      i50 = int(NR * 0.5); if (i50 < 1) i50 = 1; i90 = int(NR * 0.9); if (i90 < 1) i90 = 1
      printf "{\"cores\": %d, \"run_seconds\": %d, \"samples_5s\": %d, \"busy_mean\": %.3f, \"busy_p50\": %.3f, \"busy_p90\": %.3f, \"saturated_share\": %.3f, \"server_cpu_seconds\": %s, \"network_cpu_seconds\": %s, \"postgres_cpu_seconds\": %s, \"note\": \"busy share of all cores per 5 s sample; saturated = at least 95%% busy; what the three processes do not account for is mostly k6, the load generator, on the same machine\"}\n",
        cores, secs, NR, sum / NR, v[i50], v[i90], (sat + 0) / NR, server, sim, pg
    }' "$cpu_samples.busy")"
  rm -f "$cpu_samples.busy"
  # Never lose a run's result to its own bookkeeping.
  jq -e . <<<"$cpu_use" >/dev/null 2>&1 || cpu_use='{"note": "the CPU record could not be read"}'
fi
rm -f "$cpu_samples"
set -e
# The network's own view of the run (D-093): its write path (batch sizes, durable-commit
# times, queue waits, sheds) and its listener's (cap waits, refusals). Read before the sim
# stops; never lose a run's result to its own bookkeeping.
sim_stats="$(curl -fsS -m 5 -H "authorization: Bearer $sim_token" \
  "http://127.0.0.1:${sim_port}/v1/stats" 2>/dev/null || true)"
jq -e . <<<"${sim_stats:-}" >/dev/null 2>&1 || sim_stats='{"note": "the network stats could not be read"}'
load_after="$(uptime | sed 's/.*load averages*: *//')"
lock_waits=null
if docker compose -f deploy/compose.yaml ps -q postgres >/dev/null 2>&1; then
  lock_waits="$(docker compose -f deploy/compose.yaml logs --no-log-prefix --since "$started_iso" postgres 2>&1 \
    | grep -c 'still waiting for' || true)"
fi
rss_kb="$(ps -o rss= -p "$server" | tr -d ' ' || echo 0)"

if [ "$(uname -s)" = "Darwin" ]; then
  cpu="$(sysctl -n machdep.cpu.brand_string)"; cores="$(sysctl -n hw.ncpu)"
  mem_gb="$(( $(sysctl -n hw.memsize) / 1073741824 ))"; os="macOS $(sw_vers -productVersion)"
else
  cpu="$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"; cores="$(nproc)"
  mem_gb="$(( $(grep MemTotal /proc/meminfo | awk '{print $2}') / 1048576 ))"; os="$(uname -sr)"
fi
commit="$(git rev-parse HEAD)"
# Untracked files count too: new code that is not committed is still not the commit.
dirty=false; [ -z "$(git status --porcelain -- . ':!perf/results' ':!target')" ] || dirty=true

out="${outdir}/${stamp}-${scenario}-${mode}.json"
jq -n \
  --arg scenario "$scenario" --arg mode "$mode" --arg stamp "$stamp" \
  --arg commit "$commit" --argjson dirty "$dirty" \
  --arg cpu "$cpu" --arg cores "$cores" --arg mem_gb "$mem_gb" --arg os "$os" \
  --arg load_before "$load_before" --arg load_after "$load_after" \
  --arg rss_kb "$rss_kb" --argjson k6_status "$k6_status" --argjson lock_waits "$lock_waits" \
  --argjson cpu_use "$cpu_use" \
  --argjson sim_stats "$sim_stats" \
  --slurpfile summary "$summary" '
  {
    scenario: $scenario, mode: $mode, started_utc: $stamp,
    commit: $commit, tree_dirty: $dirty,
    machine: {cpu: $cpu, cores: ($cores|tonumber), memory_gb: ($mem_gb|tonumber), os: $os,
              load_average_before: $load_before, load_average_after: $load_after,
              note: "same-machine load generator: a development observation, not capacity"},
    server_rss_kb_at_end: ($rss_kb|tonumber),
    cpu: $cpu_use,
    network_stats: $sim_stats,
    postgres_lock_waits_over_50ms: $lock_waits,
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
jq '{scenario, mode, commit, thresholds_passed, requests, requests_per_second, failed_rate, dropped_iterations, postgres_lock_waits_over_50ms, latency_ms, machine: {cpu: .machine.cpu, load_average_before: .machine.load_average_before}}' "$out"
exit "$k6_status"
