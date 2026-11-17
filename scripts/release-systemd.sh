#!/usr/bin/env bash
# The install of docs/OPERATING.md under systemd, for real (D-071): as root on a Linux host
# with systemd, after scripts/release-smoke.sh built the release, against the host's own
# PostgreSQL 18 as Ubuntu's packages install it (scripts/ci-postgresql.sh, D-082): setup as
# `postgres` over the Unix socket, the services over 127.0.0.1 with passwords. CI runs it on
# every push ("release" job); it changes /opt, /etc and the system's units, so it is not for
# a developer's machine. SIMULATED MONEY ONLY.
set -euo pipefail
cd "$(dirname "$0")/.."
[ "$(id -u)" = 0 ] || { echo "release-systemd: run as root on a disposable Linux host (CI)" >&2; exit 1; }
command -v systemctl >/dev/null || { echo "release-systemd: needs systemd" >&2; exit 1; }
pg_port="${PS_HOST_PG_PORT:?PS_HOST_PG_PORT, the port of the host PostgreSQL (scripts/ci-postgresql.sh)}"
host_port="127.0.0.1:$pg_port"
# docs/OPERATING.md's admin URL for a stock install: the Unix socket, as the OS user
# postgres (peer authentication, no password). The port only because CI's runner may
# already have another cluster on 5432.
admin_url="postgres:///postgres?host=/var/run/postgresql&port=$pg_port"
# Built by scripts/release-smoke.sh (no cargo here: under sudo, root has no toolchain).
release="${RELEASE_DIR:-target/release}"
[ -x "$release/sandbox-server" ] || { echo "release-systemd: no release build in $release (run scripts/release-smoke.sh first)" >&2; exit 1; }

secret() { od -An -N32 -tx1 /dev/urandom | tr -d ' \n'; }
set_var() {
  grep -q "^$2=" "$1" || { echo "release-systemd: $1 has no $2 setting" >&2; exit 1; }
  sed -i "s|^$2=.*|$2=$3|" "$1"
}
on_failure() {
  status=$?
  if [ "$status" -ne 0 ]; then
    journalctl --no-pager -n 80 -u payment-sandbox-migrate -u payment-sandbox-sim -u payment-sandbox-server -u payment-sandbox-backup || true
  fi
  systemctl stop payment-sandbox-server payment-sandbox-sim 2>/dev/null || true
  exit "$status"
}
trap on_failure EXIT

# Section 1 of docs/OPERATING.md.
id payment-sandbox >/dev/null 2>&1 || useradd --system --home /var/lib/payment-sandbox --shell /usr/sbin/nologin payment-sandbox
install -d -m 0755 /opt/payment-sandbox/bin
install -m 0755 "$release/sandbox-server" "$release/sandbox-sim" "$release/sandbox-cli" \
  deploy/backup/payment-sandbox-backup /opt/payment-sandbox/bin/
rm -rf /opt/payment-sandbox/checkout /opt/payment-sandbox/dashboard
cp -R apps/checkout/build /opt/payment-sandbox/checkout
cp -R apps/dashboard/build /opt/payment-sandbox/dashboard
install -d -m 0700 /etc/payment-sandbox
for f in server sim migrate; do install -m 0600 "deploy/env/$f.env.example" "/etc/payment-sandbox/$f.env"; done
sim_token="$(secret)" bootstrap="$(secret)"
# The roles' passwords are the operator's own, as the guide says (never the development ones).
owner_password="$(secret)" app_password="$(secret)"
set_var /etc/payment-sandbox/sim.env PS_SIM_TOKEN "$sim_token"
set_var /etc/payment-sandbox/server.env PS_SIM_TOKEN "$sim_token"
set_var /etc/payment-sandbox/server.env PS_BOOTSTRAP_TOKEN "$bootstrap"
set_var /etc/payment-sandbox/server.env PS_SECRET_KEY "$(secret)"
set_var /etc/payment-sandbox/server.env PS_DATABASE_URL "postgres://ps_app:$app_password@$host_port/payment_sandbox"
set_var /etc/payment-sandbox/migrate.env PS_MIGRATE_DATABASE_URL "postgres://ps_owner:$owner_password@$host_port/payment_sandbox"
install -m 0600 deploy/env/backup.env.example /etc/payment-sandbox/backup.env
install -m 0644 deploy/systemd/*.service deploy/systemd/*.timer /etc/systemd/system/
systemd-analyze verify /etc/systemd/system/payment-sandbox-*.service /etc/systemd/system/payment-sandbox-*.timer

# The guide's earlier form, a TCP URL without a password, cannot work on a stock install
# (v1 review M8): scram-sha-256 on 127.0.0.1, and postgres has no password.
if PS_ADMIN_DATABASE_URL="postgres://postgres@$host_port/postgres" \
  /opt/payment-sandbox/bin/sandbox-server setup >/dev/null 2>&1; then
  echo "release-systemd: the passwordless TCP admin URL worked; update docs/OPERATING.md" >&2
  exit 1
fi
sudo -u postgres env PS_ADMIN_DATABASE_URL="$admin_url" \
  PS_OWNER_PASSWORD="$owner_password" PS_APP_PASSWORD="$app_password" \
  /opt/payment-sandbox/bin/sandbox-server setup
systemctl daemon-reload
systemctl start payment-sandbox-migrate
systemctl start payment-sandbox-sim payment-sandbox-server

# The env files are root's alone (D-081): no service user can read them, least of all the
# schema owner's password in migrate.env; the migrate unit runs as its own throwaway user.
fail() { echo "release-systemd: $*" >&2; exit 1; }
[ "$(stat -c '%a %U %G' /etc/payment-sandbox)" = "700 root root" ] || fail "/etc/payment-sandbox is $(stat -c '%a %U %G' /etc/payment-sandbox)"
for f in server sim migrate backup; do
  mode="$(stat -c '%a %U %G' "/etc/payment-sandbox/$f.env")"
  [ "$mode" = "600 root root" ] || fail "$f.env is $mode, not 600 root root"
  if runuser -u payment-sandbox -- test -r "/etc/payment-sandbox/$f.env"; then
    fail "the server's user can read $f.env"
  fi
done
[ "$(systemctl show -p DynamicUser --value payment-sandbox-migrate)" = yes ] || fail "the migrate unit runs as a fixed user"

# Section 2: first run.
for _ in $(seq 1 100); do
  curl -fsS http://127.0.0.1:56080/readyz >/dev/null 2>&1 && break
  sleep 0.2
done
curl -fsS http://127.0.0.1:56080/readyz | jq -e '.status == "ready"' >/dev/null
key="$(curl -fsS -X POST http://127.0.0.1:56080/v1/projects -H "Authorization: Bearer $bootstrap" \
  -H 'Content-Type: application/json' -d '{"slug": "team-store", "name": "Team store"}' | jq -r .api_key.secret)"
PS_API_KEY="$key" /opt/payment-sandbox/bin/sandbox-cli status
PS_API_KEY="$key" /opt/payment-sandbox/bin/sandbox-cli payments create --amount 42.00 --currency USD
# The units restart a killed service: a new process (MainPID changes, NRestarts grows) that
# becomes ready, not the old one answering before it died.
old_pid="$(systemctl show -p MainPID --value payment-sandbox-server)"
old_restarts="$(systemctl show -p NRestarts --value payment-sandbox-server)"
systemctl kill --signal=SIGKILL payment-sandbox-server
restarted=no
for _ in $(seq 1 150); do
  pid="$(systemctl show -p MainPID --value payment-sandbox-server)"
  restarts="$(systemctl show -p NRestarts --value payment-sandbox-server)"
  if [ "$pid" != 0 ] && [ "$pid" != "$old_pid" ] && [ "$restarts" -gt "$old_restarts" ] \
    && curl -fsS http://127.0.0.1:56080/readyz 2>/dev/null | jq -e '.status == "ready"' >/dev/null; then
    restarted=yes
    break
  fi
  sleep 0.2
done
[ "$restarted" = yes ] || fail "the server was not restarted (MainPID $old_pid -> $pid, NRestarts $old_restarts -> $restarts)"

# Section 4: the backup (live dump, the network's store, the env files), then the restore
# into a fresh database with the store put back, as the guide says (D-083).
cli() { PS_API_KEY="$key" /opt/payment-sandbox/bin/sandbox-cli "$@"; }
paid="$(cli --json payments create --amount 7.00 --currency USD | jq -r .payment.id)"
systemctl enable --now payment-sandbox-backup.timer
systemctl list-timers --all --no-pager payment-sandbox-backup.timer | grep -q payment-sandbox-backup.service \
  || fail "the backup timer is not scheduled"
systemctl start payment-sandbox-backup
backups=/var/backups/payment-sandbox
[ "$(stat -c '%a %U' "$backups")" = "700 root" ] || fail "$backups is $(stat -c '%a %U' "$backups")"
dump="$(find "$backups" -name 'db-*.dump' | sort | tail -n 1)"
store="$(find "$backups" -name 'sim-*.redb' | sort | tail -n 1)"
config="$(find "$backups" -name 'etc-*.tar' | sort | tail -n 1)"
[ -s "$dump" ] && [ -s "$store" ] && [ -s "$config" ] || fail "the backup is incomplete: $(ls "$backups")"
tar -tf "$config" | grep -q 'payment-sandbox/server.env' || fail "the backup lacks the env files"
systemctl is-active --quiet payment-sandbox-sim || fail "the backup left the network stopped"
journalctl --no-pager -u payment-sandbox-backup | grep -q 'backup ok' || fail "the backup did not report ok"

systemctl stop payment-sandbox-server
sudo -u postgres env PS_ADMIN_DATABASE_URL="$admin_url" PS_DATABASE_NAME=payment_sandbox_restored \
  /opt/payment-sandbox/bin/sandbox-server setup
pg_restore --single-transaction --exit-on-error --no-owner --role=ps_owner \
  --dbname="postgres://ps_owner:$owner_password@$host_port/payment_sandbox_restored" "$dump"
set_var /etc/payment-sandbox/server.env PS_DATABASE_URL "postgres://ps_app:$app_password@$host_port/payment_sandbox_restored"
set_var /etc/payment-sandbox/migrate.env PS_MIGRATE_DATABASE_URL "postgres://ps_owner:$owner_password@$host_port/payment_sandbox_restored"
systemctl stop payment-sandbox-sim
install -o payment-sandbox -g payment-sandbox -m 0600 "$store" /var/lib/payment-sandbox/sim.redb
systemctl start payment-sandbox-sim payment-sandbox-migrate payment-sandbox-server
for _ in $(seq 1 150); do
  curl -fsS http://127.0.0.1:56080/readyz 2>/dev/null | jq -e '.status == "ready"' >/dev/null && break
  sleep 0.2
done
curl -fsS http://127.0.0.1:56080/readyz | jq -e '.status == "ready"' >/dev/null || fail "not ready after the restore"
cli payments get "$paid" >/dev/null || fail "the payment made before the backup is missing after the restore"
cli reconcile --network >/dev/null || fail "the restored database and store do not reconcile"
cli payments create --amount 3.00 --currency USD >/dev/null || fail "no new payment after the restore"
echo "release systemd ok: the units install, start, serve, restart (pid $old_pid -> $pid), back up and restore"
