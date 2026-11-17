#!/usr/bin/env bash
# The browser journeys (ROADMAP M6): Playwright in Chromium against the real server (with
# its checkout and dashboard origins), the simulated network and the development store, on
# a fresh database of the local stack. Then (M7) the developer examples, run as written,
# and the TypeScript SDK's live tests, against the same processes. SIMULATED MONEY ONLY.
#
#   scripts/e2e.sh                 build the apps and the binaries, run every journey, the
#                                  examples (examples/) and the SDK's live tests
#   scripts/e2e.sh -g "keyboard"   extra arguments go to `playwright test` (journeys only)
#
# Ports 56460-56466 on 127.0.0.1: API, network, checkout, dashboard (reached on its own
# host name, dashboard.localhost, D-065), store, a merchant page that frames the
# checkout (e2e/merchant.mjs, reached as localhost), and the SDK test's webhook receiver.
# Everything
# this starts is stopped by PID, and its database and stores are removed, when it exits.
# PS_E2E_PERF_OUT=<file> also runs the page-load measurement (scripts/web-perf.sh uses it).
set -euo pipefail
cd "$(dirname "$0")/.."

base_port="${PS_E2E_PORT:-56460}"
api_port="$base_port" sim_port="$((base_port + 1))" checkout_port="$((base_port + 2))"
dashboard_port="$((base_port + 3))" store_port="$((base_port + 4))" merchant_port="$((base_port + 5))"
sdk_hook_port="$((base_port + 6))"

if [ -z "${PS_ADMIN_DATABASE_URL:-}" ]; then
  scripts/stack.sh up >/dev/null
  eval "$(scripts/stack.sh env)"
fi
admin_url="${PS_ADMIN_DATABASE_URL:?PS_ADMIN_DATABASE_URL (scripts/stack.sh env)}"
host_port="$(sed -E 's#^postgres://[^@]+@([^/]+)/.*#\1#' <<<"$admin_url")"

[ -d node_modules ] || npm ci --no-audit --no-fund
npm run build --workspace @sandbox/checkout --workspace @sandbox/dashboard --workspace @sandbox/sdk >/dev/null
cargo build --locked -p sandbox-server -p sandbox-sim -p sandbox-dev-store -p sandbox-cli
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
PS_WEBHOOK_PORTS="$store_port,$sdk_hook_port" PS_SECRET_KEY="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')" \
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
  E2E_DASHBOARD="http://dashboard.localhost:${dashboard_port}" E2E_STORE="http://127.0.0.1:${store_port}" \
  E2E_MERCHANT="http://localhost:${merchant_port}" E2E_BOOTSTRAP="$bootstrap" \
  E2E_STORE_TOKEN="$store_control" E2E_SIM_CONTROL="$sim_control" \
  npx playwright test "$@")

if [ "$#" -eq 0 ]; then
  # The developer examples, exactly as docs/DEVELOPERS.md shows them (scripts/check-doc-examples.mjs
  # keeps the two identical), each on a fresh project.
  new_key() {
    curl -fsS -X POST "http://127.0.0.1:${api_port}/v1/projects" -H "Authorization: Bearer $bootstrap" \
      -H 'Content-Type: application/json' -d "{\"slug\": \"$1-$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')\", \"name\": \"$1\"}" \
      | jq -r .api_key.secret
  }
  PATH="$bin:$PATH" PS_API_URL="http://127.0.0.1:${api_port}" PS_API_KEY="$(new_key cli-example)" \
    examples/cli-quickstart.sh >"$work/cli-example.log" 2>&1 || { cat "$work/cli-example.log"; exit 1; }
  PS_API_URL="http://127.0.0.1:${api_port}" PS_API_KEY="$(new_key sdk-example)" \
    node examples/sdk-purchase.mjs >"$work/sdk-example.log" 2>&1 || { cat "$work/sdk-example.log"; exit 1; }
  # The hosted checkout example: the store creates a session and waits; a stand-in payer
  # (e2e/payer.mjs) opens the printed link and approves, as the page would.
  PS_API_URL="http://127.0.0.1:${api_port}" PS_API_KEY="$(new_key checkout-example)" \
    node examples/checkout.mjs >"$work/checkout-example.log" 2>&1 &
  example=$!
  pids+=("$example")
  link=""
  for _ in $(seq 1 100); do
    link="$(grep -o 'http://[^ ]*/c/[^ ]*' "$work/checkout-example.log" 2>/dev/null | head -n 1 || true)"
    [ -n "$link" ] && break
    sleep 0.1
  done
  [ -n "$link" ] || { cat "$work/checkout-example.log"; exit 1; }
  node e2e/payer.mjs "$link" >"$work/payer.log" 2>&1 || { cat "$work/payer.log"; exit 1; }
  wait "$example" || { cat "$work/checkout-example.log"; exit 1; }
  grep -q '^checkout paid' "$work/checkout-example.log" || { cat "$work/checkout-example.log"; exit 1; }
  echo "examples ok: examples/cli-quickstart.sh, examples/sdk-purchase.mjs and examples/checkout.mjs"
  # The SDK against the real server (packages/sdk-ts/test/live).
  E2E_API="http://127.0.0.1:${api_port}" E2E_BOOTSTRAP="$bootstrap" E2E_SDK_WEBHOOK_PORT="$sdk_hook_port" \
    npm run test:live --workspace @sandbox/sdk
fi

# Nothing a journey typed, and no credential, reaches the server's log.
if grep -qE "cs_sim_[a-p]{16}|sk_sim_[0-9a-f]{16}|$sim_control|4242 ?4242" "$work/server.log"; then
  echo "e2e: a secret or card-like input reached the server log" >&2
  exit 1
fi
echo "e2e ok"
