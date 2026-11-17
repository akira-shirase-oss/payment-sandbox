#!/usr/bin/env bash
# The browser journeys (ROADMAP M6): Playwright in Chromium against the real server (with
# its checkout and dashboard origins), the simulated network and the development store, on
# a fresh database of the local stack. SIMULATED MONEY ONLY.
#
#   scripts/e2e.sh                 build the apps and the binaries, run every journey
#   scripts/e2e.sh -g "keyboard"   extra arguments go to `playwright test`
#
# Ports 56460-56465 on 127.0.0.1: API, network, checkout, dashboard, store, and a merchant
# page that frames the checkout (e2e/merchant.mjs, reached as localhost). Everything
# this starts is stopped by PID, and its database and stores are removed, when it exits.
# PS_E2E_PERF_OUT=<file> also runs the page-load measurement (scripts/web-perf.sh uses it).
set -euo pipefail
cd "$(dirname "$0")/.."

base_port="${PS_E2E_PORT:-56460}"
api_port="$base_port" sim_port="$((base_port + 1))" checkout_port="$((base_port + 2))"
dashboard_port="$((base_port + 3))" store_port="$((base_port + 4))" merchant_port="$((base_port + 5))"

if [ -z "${PS_ADMIN_DATABASE_URL:-}" ]; then
  scripts/stack.sh up >/dev/null
  eval "$(scripts/stack.sh env)"
fi
admin_url="${PS_ADMIN_DATABASE_URL:?PS_ADMIN_DATABASE_URL (scripts/stack.sh env)}"
host_port="$(sed -E 's#^postgres://[^@]+@([^/]+)/.*#\1#' <<<"$admin_url")"

[ -d node_modules ] || npm ci --no-audit --no-fund
npm run build --workspace @sandbox/checkout --workspace @sandbox/dashboard >/dev/null
cargo build --locked -p sandbox-server -p sandbox-sim -p sandbox-dev-store
bin="$(cargo metadata --format-version 1 --no-deps --locked | jq -r .target_directory)/debug"

work="$(mktemp -d "${TMPDIR:-/tmp}/ps-e2e.XXXXXX")"
db="ps_e2e_$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
pids=()
cleanup() {
  status=$?
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  if [ -n "${PS_PG_CONTAINER:-}" ]; then
    docker exec "$PS_PG_CONTAINER" psql -q -U sandbox -d sandbox -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null 2>&1 || true
  else
    docker compose -f deploy/compose.yaml exec -T postgres \
      psql -q -U sandbox -d sandbox -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null 2>&1 || true
  fi
  if [ "$status" -ne 0 ]; then
    for log in "$work"/*.log; do echo "--- $log (last 30 lines)"; tail -30 "$log"; done
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT

# The roles exist already or are created; the database is this run's own (D-038).
PS_ADMIN_DATABASE_URL="$admin_url" PS_DATABASE_NAME="$db" "$bin/sandbox-server" setup >/dev/null
PS_MIGRATE_DATABASE_URL="postgres://ps_owner:ps-owner-dev@${host_port}/${db}" "$bin/sandbox-server" migrate >/dev/null

random() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }
sim_token="sim-$(random)" sim_control="simctl-$(random)" store_control="store-$(random)" bootstrap="e2e-$(random)"

PS_PROFILE=dev PS_SIM_TOKEN="$sim_token" PS_SIM_CONTROL_TOKEN="$sim_control" PS_SIM_LOST_REPLY_HOLD_MS=3000 \
PS_SIM_LISTEN="127.0.0.1:${sim_port}" PS_SIM_DB="$work/sim.redb" PS_SIM_LOG=warn \
  "$bin/sandbox-sim" >"$work/sim.log" 2>&1 &
pids+=($!)
PS_PROFILE=dev PS_STORE_CONTROL=on PS_STORE_CONTROL_TOKEN="$store_control" \
PS_STORE_LISTEN="127.0.0.1:${store_port}" PS_STORE_DB="$work/store.redb" \
  "$bin/sandbox-dev-store" >"$work/store.log" 2>&1 &
pids+=($!)
PS_PROFILE=dev PS_DATABASE_URL="postgres://ps_app:ps-app-dev@${host_port}/${db}" \
PS_LISTEN="127.0.0.1:${api_port}" PS_SIM_URL="http://127.0.0.1:${sim_port}" PS_SIM_TOKEN="$sim_token" \
PS_SIM_TIMEOUT_MS=1000 PS_RECONCILE_INTERVAL_MS=300 PS_DELIVERY_POLL_MS=100 \
PS_WEBHOOK_PORTS="$store_port" PS_SECRET_KEY="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')" \
PS_BOOTSTRAP_TOKEN="$bootstrap" PS_LOG=warn \
PS_CHECKOUT_DIR=apps/checkout/build PS_CHECKOUT_LISTEN="127.0.0.1:${checkout_port}" \
PS_DASHBOARD_DIR=apps/dashboard/build PS_DASHBOARD_LISTEN="127.0.0.1:${dashboard_port}" \
  "$bin/sandbox-server" >"$work/server.log" 2>&1 &
pids+=($!)
node e2e/merchant.mjs "$merchant_port" >"$work/merchant.log" 2>&1 &
pids+=($!)

for _ in $(seq 1 200); do
  curl -fsS "http://127.0.0.1:${api_port}/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS "http://127.0.0.1:${api_port}/readyz" >/dev/null

(cd e2e && \
  E2E_API="http://127.0.0.1:${api_port}" E2E_CHECKOUT="http://127.0.0.1:${checkout_port}" \
  E2E_DASHBOARD="http://127.0.0.1:${dashboard_port}" E2E_STORE="http://127.0.0.1:${store_port}" \
  E2E_MERCHANT="http://localhost:${merchant_port}" E2E_BOOTSTRAP="$bootstrap" \
  E2E_STORE_TOKEN="$store_control" E2E_SIM_CONTROL="$sim_control" \
  npx playwright test "$@")

# Nothing a journey typed, and no credential, reaches the server's log.
if grep -qE "cs_sim_[a-p]{16}|sk_sim_[0-9a-f]{16}|$sim_control|4242 ?4242" "$work/server.log"; then
  echo "e2e: a secret or card-like input reached the server log" >&2
  exit 1
fi
echo "e2e ok"
