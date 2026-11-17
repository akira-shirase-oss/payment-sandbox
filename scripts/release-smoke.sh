#!/usr/bin/env bash
# The production path, end to end (D-071; docs/OPERATING.md). SIMULATED MONEY ONLY.
#
#   scripts/release-smoke.sh
#
# 1. release builds of the binaries and the two web apps;
# 2. an install into a scratch prefix laid out as docs/OPERATING.md lays out /opt and /etc,
#    with configuration rendered from deploy/env/*.env.example (only paths, ports, database
#    names and freshly generated secrets are filled in: a template missing a setting fails);
# 3. `sandbox-server setup` and the migrate unit's command, on a fresh database;
# 4. the network and the server started from those env files in the `shared` profile, with
#    public https origins for the checkout and the dashboard, as behind deploy/Caddyfile;
# 5. checks: readiness; the test control planes absent; the dashboard answering only its
#    own host name; checkout URLs on the public origin; then the CLI quickstart
#    (examples/cli-quickstart.sh) run against it as written;
# 6. the API behind Caddy over https (its own local CA), the CLI trusting it with --ca-cert,
#    and refusing it plainly without (D-074).
#
# Ports 56470-56475 on 127.0.0.1. Everything started is stopped by PID, and the database
# and files are removed, on exit. CI also installs the systemd units and starts them
# (.github/workflows/ci.yml, job "release").
set -euo pipefail
cd "$(dirname "$0")/.."

api_port=56470 sim_port=56471 checkout_port=56472 dashboard_port=56473
https_port=56474 caddy_http_port=56475
pay_host=pay.sandbox.test dash_host=dashboard.sandbox.test

if [ -z "${PS_ADMIN_DATABASE_URL:-}" ]; then
  scripts/stack.sh up >/dev/null
  eval "$(scripts/stack.sh env)"
fi
admin_url="${PS_ADMIN_DATABASE_URL:?PS_ADMIN_DATABASE_URL (scripts/stack.sh env)}"
host_port="$(sed -E 's#^postgres://[^@]+@([^/]+)/.*#\1#' <<<"$admin_url")"

cargo build --release --locked -p sandbox-server -p sandbox-sim -p sandbox-cli
[ -d node_modules ] || npm ci --no-audit --no-fund
npm run build --workspace @sandbox/checkout --workspace @sandbox/dashboard >/dev/null
release="$(cargo metadata --format-version 1 --no-deps --locked | jq -r .target_directory)/release"

root="$(mktemp -d "${TMPDIR:-/tmp}/ps-release.XXXXXX")"
db="ps_release_$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
pids=()
cleanup() {
  status=$?
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  psql_admin "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null 2>&1 || true
  if [ "$status" -ne 0 ]; then
    for log in "$root"/*.log; do [ -f "$log" ] && { echo "--- $log (last 30 lines)"; tail -30 "$log"; }; done
  fi
  rm -rf "$root"
  exit "$status"
}
trap cleanup EXIT
psql_admin() {
  if [ -n "${PS_PG_CONTAINER:-}" ]; then
    docker exec "$PS_PG_CONTAINER" psql -q -U sandbox -d sandbox -c "$1"
  else
    docker compose -f deploy/compose.yaml exec -T postgres psql -q -U sandbox -d sandbox -c "$1"
  fi
}

# 2. The install, as docs/OPERATING.md describes it, under $root instead of /.
mkdir -p "$root/opt/bin" "$root/etc" "$root/var"
install -m 0755 "$release/sandbox-server" "$release/sandbox-sim" "$release/sandbox-cli" "$root/opt/bin/"
cp -R apps/checkout/build "$root/opt/checkout"
cp -R apps/dashboard/build "$root/opt/dashboard"
for f in server sim migrate; do install -m 0600 "deploy/env/$f.env.example" "$root/etc/$f.env"; done
secret() { od -An -N32 -tx1 /dev/urandom | tr -d ' \n'; }
# Sets KEY=VALUE in an env file; the template must already name KEY.
set_var() {
  grep -q "^$2=" "$1" || { echo "release-smoke: $1 has no $2 setting" >&2; exit 1; }
  local escaped
  escaped="$(printf '%s' "$3" | sed 's/[&|]/\\&/g')"
  sed -i.bak "s|^$2=.*|$2=$escaped|" "$1" && rm -f "$1.bak"
}
sim_token="$(secret)"
set_var "$root/etc/sim.env" PS_SIM_TOKEN "$sim_token"
set_var "$root/etc/sim.env" PS_SIM_LISTEN "127.0.0.1:$sim_port"
set_var "$root/etc/sim.env" PS_SIM_DB "$root/var/sim.redb"
set_var "$root/etc/server.env" PS_SIM_TOKEN "$sim_token"
set_var "$root/etc/server.env" PS_SIM_URL "http://127.0.0.1:$sim_port"
set_var "$root/etc/server.env" PS_LISTEN "127.0.0.1:$api_port"
set_var "$root/etc/server.env" PS_DATABASE_URL "postgres://ps_app:ps-app-dev@$host_port/$db"
bootstrap="$(secret)"
set_var "$root/etc/server.env" PS_BOOTSTRAP_TOKEN "$bootstrap"
set_var "$root/etc/server.env" PS_SECRET_KEY "$(secret)"
set_var "$root/etc/server.env" PS_CHECKOUT_DIR "$root/opt/checkout"
set_var "$root/etc/server.env" PS_CHECKOUT_LISTEN "127.0.0.1:$checkout_port"
set_var "$root/etc/server.env" PS_CHECKOUT_ORIGIN "https://$pay_host"
set_var "$root/etc/server.env" PS_DASHBOARD_DIR "$root/opt/dashboard"
set_var "$root/etc/server.env" PS_DASHBOARD_LISTEN "127.0.0.1:$dashboard_port"
set_var "$root/etc/server.env" PS_DASHBOARD_ORIGIN "https://$dash_host"
set_var "$root/etc/migrate.env" PS_MIGRATE_DATABASE_URL "postgres://ps_owner:ps-owner-dev@$host_port/$db"
# Nothing generic is left blank.
if grep -nE '^[A-Z_]+=$' "$root"/etc/*.env; then
  echo "release-smoke: a setting above is still empty" >&2
  exit 1
fi

# Runs a binary with exactly one env file's settings (plus PATH), as systemd's
# EnvironmentFile does. `exec` all the way down, so a background job's $! is the binary's
# own PID and the cleanup stops it (without it, the first version of this script left the
# server and the network running after it exited).
with_env() {
  local file="$1"
  shift
  exec env -i PATH="$PATH" HOME="$root" bash -c 'set -a; . "$0"; set +a; exec "$@"' "$file" "$@"
}

# Every binary says what it is (D-073).
for b in sandbox-server sandbox-sim sandbox-cli; do
  "$root/opt/bin/$b" --version | grep -q "SIMULATED MONEY ONLY" || { echo "release-smoke: $b --version lacks the notice" >&2; exit 1; }
done

# 3. The database: setup as the administrator, then the migrate unit's command (in a
# subshell, since with_env replaces the shell it runs in).
PS_ADMIN_DATABASE_URL="$admin_url" PS_DATABASE_NAME="$db" "$root/opt/bin/sandbox-server" setup >/dev/null
(with_env "$root/etc/migrate.env" "$root/opt/bin/sandbox-server" migrate >/dev/null)

# 4. The services, from their env files, on ports nothing else holds.
for p in "$api_port" "$sim_port" "$checkout_port" "$dashboard_port"; do
  if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$p/"; then
    echo "release-smoke: port $p is already in use; stop what holds it" >&2
    exit 1
  fi
done
with_env "$root/etc/sim.env" "$root/opt/bin/sandbox-sim" >"$root/sim.log" 2>&1 &
pids+=($!)
with_env "$root/etc/server.env" "$root/opt/bin/sandbox-server" serve >"$root/server.log" 2>&1 &
pids+=($!)
api="http://127.0.0.1:$api_port"
for _ in $(seq 1 200); do
  curl -fsS "$api/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS "$api/readyz" | jq -e '.status == "ready"' >/dev/null

# 5. Checks.
fail() { echo "release-smoke: $*" >&2; exit 1; }
curl -fsS "$api/v1/sandbox" | jq -e '.simulated == true' >/dev/null || fail "/v1/sandbox is not simulated"
# The shared profile has no test control plane: the network's fault routes do not exist.
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $sim_token" \
  -H 'Content-Type: application/json' -d '{}' "http://127.0.0.1:$sim_port/v1/faults")"
[ "$code" = 404 ] || fail "the network's fault controls answered $code in the shared profile"
# The dashboard answers only on its own host name; a page elsewhere is sent there.
page="$(curl -s -D - -o /dev/null -H "Host: $dash_host" "http://127.0.0.1:$dashboard_port/")"
grep -qi '^content-security-policy:.*frame-ancestors' <<<"$page" || fail "the dashboard page has no policy"
grep -qi '^sandbox-simulated: true' <<<"$page" || fail "the dashboard page is not labelled simulated"
moved="$(curl -s -D - -o /dev/null -H "Host: 127.0.0.1:$dashboard_port" "http://127.0.0.1:$dashboard_port/payments")"
grep -qi "^location: https://$dash_host/payments" <<<"$moved" || fail "the dashboard did not send another host name to its own"
# A project, and checkout sessions on the public origin.
created="$(curl -fsS -X POST "$api/v1/projects" -H "Authorization: Bearer $bootstrap" \
  -H 'Content-Type: application/json' -d '{"slug": "release-shop", "name": "Release shop"}')"
key="$(jq -r .api_key.secret <<<"$created")"
session="$(curl -fsS -X POST "$api/v1/checkout_sessions" -H "Authorization: Bearer $key" \
  -H 'Content-Type: application/json' -d '{"amount": 1000, "currency": "USD"}')"
jq -e --arg origin "https://$pay_host/c/" '.url | startswith($origin)' <<<"$session" >/dev/null \
  || fail "checkout URLs do not name the public origin"
# The CLI quickstart, as written, against the release.
PATH="$root/opt/bin:$PATH" PS_API_URL="$api" PS_API_KEY="$key" examples/cli-quickstart.sh >"$root/quickstart.log" 2>&1 \
  || { cat "$root/quickstart.log"; fail "the CLI quickstart failed against the release"; }
# 6. The API behind an HTTPS proxy, as on a shared host (D-074): Caddy with its own local
# certificate authority, and the CLI over https, trusting that authority with --ca-cert.
scripts/install-caddy.sh "$root/bin" >/dev/null
for p in "$https_port" "$caddy_http_port"; do
  if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$p/"; then
    fail "port $p is already in use; stop what holds it"
  fi
done
cat >"$root/Caddyfile" <<EOF
{
	admin off
	skip_install_trust
	storage file_system $root/caddy
	http_port $caddy_http_port
	https_port $https_port
	auto_https disable_redirects
}
https://localhost:$https_port {
	tls internal
	reverse_proxy 127.0.0.1:$api_port
}
EOF
XDG_DATA_HOME="$root/xdg" XDG_CONFIG_HOME="$root/xdg" HOME="$root" \
  "$root/bin/caddy" run --config "$root/Caddyfile" --adapter caddyfile >"$root/caddy.log" 2>&1 &
pids+=($!)
ca="$root/caddy/pki/authorities/local/root.crt"
https_api="https://localhost:$https_port"
cli() { PS_API_URL="$https_api" PS_API_KEY="$key" "$root/opt/bin/sandbox-cli" "$@"; }
for _ in $(seq 1 100); do
  [ -f "$ca" ] && cli --ca-cert "$ca" status >/dev/null 2>&1 && break
  sleep 0.2
done
cli --ca-cert "$ca" status >"$root/https.log" 2>&1 || { cat "$root/https.log"; fail "the CLI could not use the API over https"; }
cli --ca-cert "$ca" payments create --amount 5.00 --currency USD >>"$root/https.log" 2>&1 \
  || { cat "$root/https.log"; fail "a payment over https failed"; }
# Without the authority: the CLI says the certificate is not trusted (exit 5), sends nothing.
set +e
cli status >"$root/untrusted.log" 2>&1
untrusted=$?
set -e
[ "$untrusted" = 5 ] && grep -q -- "--ca-cert" "$root/untrusted.log" \
  || { cat "$root/untrusted.log"; fail "an untrusted certificate was not reported plainly (exit $untrusted)"; }
# Nothing secret reached the logs.
if grep -qE "$bootstrap|$sim_token|sk_sim_[0-9a-f]{16}" "$root/server.log" "$root/sim.log"; then
  fail "a secret reached a log"
fi
echo "release smoke ok: release binaries, env files, the shared profile, the CLI quickstart and the CLI over https"
