#!/usr/bin/env bash
# Records the web apps' bundle sizes (initial JS and CSS, gzip), a page-load measurement and
# the web vitals (LCP, CLS, INP of the checkout and the dashboard's main pages,
# e2e/tests/vitals.spec.ts) in perf/results/ (ROADMAP M6, D-064, D-088), with the machine, its
# load and the commit, like every other result. The apps are their production builds
# (`npm run build`); the pages run in Chromium on loopback against the binaries
# scripts/e2e.sh builds (debug): a development observation, never capacity.
set -euo pipefail
cd "$(dirname "$0")/.."

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$(mktemp -d "${TMPDIR:-/tmp}/ps-web-perf.XXXXXX")"
trap 'rm -rf "$work"' EXIT
load_before="$(uptime | sed 's/.*load averages*: *//')"
E2E_PERF_OUT="$work/pageload.json" E2E_VITALS_OUT="$work/vitals.json" \
  scripts/e2e.sh -g "page loads|web vitals" >"$work/e2e.log" 2>&1 || {
  tail -40 "$work/e2e.log"; exit 1;
}
node scripts/web-budget.mjs --json >"$work/bundles.json"
load_after="$(uptime | sed 's/.*load averages*: *//')"
if [ "$(uname -s)" = "Darwin" ]; then
  cpu="$(sysctl -n machdep.cpu.brand_string)"; cores="$(sysctl -n hw.ncpu)"
  mem_gb="$(( $(sysctl -n hw.memsize) / 1073741824 ))"; os="macOS $(sw_vers -productVersion)"
else
  cpu="$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"; cores="$(nproc)"
  mem_gb="$(( $(grep MemTotal /proc/meminfo | awk '{print $2}') / 1048576 ))"; os="$(uname -sr)"
fi
dirty=false; [ -z "$(git status --porcelain -- . ':!perf/results' ':!target')" ] || dirty=true
chromium="$(cd e2e && npx playwright --version)"
mkdir -p perf/results
out="perf/results/${stamp}-web-v1.json"
jq -n --arg stamp "$stamp" --arg commit "$(git rev-parse HEAD)" --argjson dirty "$dirty" \
  --arg cpu "$cpu" --arg cores "$cores" --arg mem_gb "$mem_gb" --arg os "$os" \
  --arg load_before "$load_before" --arg load_after "$load_after" --arg browser "$chromium" \
  --slurpfile bundles "$work/bundles.json" --slurpfile pageload "$work/pageload.json" \
  --slurpfile vitals "$work/vitals.json" '
  { scenario: "web-v1", started_utc: $stamp, commit: $commit, tree_dirty: $dirty,
    machine: {cpu: $cpu, cores: ($cores|tonumber), memory_gb: ($mem_gb|tonumber), os: $os,
              load_average_before: $load_before, load_average_after: $load_after,
              note: "browser, servers and database on one machine: a development observation, not capacity"},
    browser: $browser, web_build: "production (npm run build)", server_build: "debug (scripts/e2e.sh)",
    bundles: $bundles[0], page_load: $pageload[0], vitals: $vitals[0] }' >"$out"
echo "result: $out"
jq '{commit, tree_dirty, machine: {load_average_before: .machine.load_average_before}, checkout_cold_ready: .page_load.checkout.cold.ready, dashboard_cold_ready: .page_load.dashboard.cold.ready, vitals: .vitals.pages}' "$out"
