#!/usr/bin/env bash
# CI only, as root on the release job's Ubuntu runner: PostgreSQL 18 installed the way the
# operator guide's host has it, from the PostgreSQL project's Debian/Ubuntu packages
# (PGDG), with their stock configuration: the `postgres` role has no password and signs in
# by peer authentication on the Unix socket, and TCP on 127.0.0.1 needs a password
# (scram-sha-256). scripts/release-systemd.sh then installs the sandbox against it exactly
# as docs/OPERATING.md says (D-082). Prints the cluster's port. SIMULATED MONEY ONLY.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "ci-postgresql: run as root on a disposable runner (CI)" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive

if grep -rqs apt.postgresql.org /etc/apt/sources.list /etc/apt/sources.list.d/; then
  apt-get update -q >/dev/null
else
  apt-get install -y -q postgresql-common >/dev/null
  # The project's own script: adds the repository and its signing key, then updates.
  /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y >/dev/null
fi
apt-get install -y -q postgresql-18 >/dev/null

port="$(pg_lsclusters -h | awk '$1 == "18" && $2 == "main" { print $3 }')"
[ -n "$port" ] || { echo "ci-postgresql: the package created no 18/main cluster" >&2; exit 1; }
pg_ctlcluster 18 main start 2>/dev/null || true
for _ in $(seq 1 100); do
  pg_isready -q -h /var/run/postgresql -p "$port" && break
  sleep 0.2
done
pg_isready -q -h /var/run/postgresql -p "$port"
# The stock rules this install relies on, unchanged: peer for postgres on the socket, a
# password on loopback TCP.
hba="$(sudo -u postgres psql -At -h /var/run/postgresql -p "$port" -c 'SHOW hba_file')"
grep -Eq '^local[[:space:]]+all[[:space:]]+postgres[[:space:]]+peer' "$hba"
grep -Eq '^host[[:space:]]+all[[:space:]]+all[[:space:]]+127\.0\.0\.1/32[[:space:]]+scram-sha-256' "$hba"
echo "$port"
