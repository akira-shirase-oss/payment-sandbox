# Operator guide

> **SIMULATED MONEY ONLY.** Payment Sandbox moves no real money and accepts no real cards.
> A deployed sandbox is a shared development and CI tool, never a payment service: no store
> may fulfill a real order from anything it says.

This guide runs one sandbox on one Linux host for a team (D-071): PostgreSQL, the simulated
network and the server as systemd services on loopback, and a reverse proxy that gives the
API, the checkout and the dashboard their HTTPS names. `scripts/release-smoke.sh` builds the
release and runs this layout end to end; CI runs it on every push.

What a shared host does not change (D-023): every process listens on loopback, and webhooks
are delivered only to receivers on this host, on the ports `PS_WEBHOOK_PORTS` lists. A store
on a developer's laptop takes payments through the API (`allowNonLocal: true` in the SDK,
`--allow-non-local` in the CLI) and reads events with `GET /v1/events`, or runs its webhook
receiver on this host.

## 1. Install

You need a Linux host with systemd, PostgreSQL 18, Caddy (or another TLS proxy), and three
DNS names pointing at it. Build on a machine with the pinned Rust toolchain and Node 22.17+:

```sh
cargo build --release --locked -p sandbox-server -p sandbox-sim -p sandbox-cli
npm ci && npm run build --workspace @sandbox/checkout --workspace @sandbox/dashboard
```

On the host, as root:

```sh
useradd --system --home /var/lib/payment-sandbox --shell /usr/sbin/nologin payment-sandbox
install -d -m 0755 /opt/payment-sandbox/bin
install -m 0755 target/release/sandbox-server target/release/sandbox-sim target/release/sandbox-cli \
  deploy/backup/payment-sandbox-backup /opt/payment-sandbox/bin/
cp -R apps/checkout/build /opt/payment-sandbox/checkout
cp -R apps/dashboard/build /opt/payment-sandbox/dashboard
install -d -m 0700 /etc/payment-sandbox
for f in server sim migrate; do install -m 0600 deploy/env/$f.env.example /etc/payment-sandbox/$f.env; done
install -m 0600 deploy/env/backup.env.example /etc/payment-sandbox/backup.env
install -m 0644 deploy/systemd/*.service deploy/systemd/*.timer /etc/systemd/system/
```

The env files are `0600 root`: systemd reads them as root before it starts each service,
so no service user needs to, and none can. In particular the server's user can never read
`migrate.env`, which holds the schema owner's password (the owner could switch off the
journal's protections, D-038/D-058); the migrate unit runs as its own throwaway user
(`DynamicUser=yes`). The release job checks both (D-081).

Fill in `/etc/payment-sandbox/*.env`. Each line is explained in the file; the empty secrets
must be generated (`openssl rand -hex 32` for each of `PS_SIM_TOKEN`, the same value in
`server.env` and `sim.env`, `PS_BOOTSTRAP_TOKEN` and `PS_SECRET_KEY`), and the database
passwords must be your own (16 to 128 letters, digits or `._~-`; `openssl rand -hex 24`).
A missing or weak value stops the process with a message naming it. `setup` never resets
an existing role's password: running it again (for a restore) is safe.

Connection budget: `PS_DB_MAX_CONNECTIONS` is the whole pool the server may hold
(default 32 — deliberately far under PostgreSQL's `max_connections`, so migrations,
backups and admin sessions always find a free connection). Reads get their own slice,
`PS_DB_READ_CONNECTIONS` (default 8), with their own acquire deadline
`PS_DB_READ_ACQUIRE_TIMEOUT_MS` (1,000 ms — then a `503` + `Retry-After`) and
statement deadline `PS_DB_READ_STATEMENT_TIMEOUT_MS` (1,000 ms); the read pool's
transactions are read-only (`default_transaction_read_only=on`), so a read path
that tries to write fails loudly (D-101). Writes keep the rest and keep
`PS_DB_ACQUIRE_TIMEOUT_MS` (1,000 ms). A write flood can hold every write connection
without a `GET`/`HEAD` ever waiting on one — a refused read is a fast named `503`
(D-100). Over the `PS_DB_MAX_CONNECTIONS` budget the `auth_epoch` listener holds
**two** more connections for its life — the `LISTEN` socket and the liveness
self-ping's own socket (D-102's addendum): a database sized with only one spare
fails every ping connect, the listener keeps clearing the key cache, and readiness
goes red.

Create the roles and the database (once, as PostgreSQL's administrator) with passwords of
your own, the same ones `migrate.env` and `server.env` use, then migrate as the owner. On
Debian or Ubuntu's PostgreSQL packages the administrator `postgres` has no password and
signs in over the Unix socket as the OS user `postgres` (peer authentication), so run
`setup` as that user with a socket URL (`setup` itself needs no file access):

```sh
sudo -u postgres env PS_ADMIN_DATABASE_URL='postgres:///postgres?host=/var/run/postgresql&user=postgres' \
  PS_OWNER_PASSWORD=your-owner-password PS_APP_PASSWORD=your-app-password \
  /opt/payment-sandbox/bin/sandbox-server setup
systemctl daemon-reload
systemctl start payment-sandbox-migrate
systemctl enable --now payment-sandbox-sim payment-sandbox-server payment-sandbox-backup.timer
```

With a password for the administrator instead (another distribution, a managed server),
use `PS_ADMIN_DATABASE_URL=postgres://postgres:...@127.0.0.1:5432/postgres`. Add
`&port=5433` to the socket URL if your cluster is not on 5432. Keep `user=postgres` in it:
without a user the client falls back to a name of its own, which peer authentication
refuses. The services always connect
over `127.0.0.1` with their passwords (`scram-sha-256` in the stock `pg_hba.conf`). CI's
release job installs PostgreSQL 18 from the PostgreSQL project's Ubuntu packages and runs
exactly this (D-082).

Point the proxy at the three loopback ports: `deploy/Caddyfile` is a complete example
(replace the names). `PS_CHECKOUT_ORIGIN` and `PS_DASHBOARD_ORIGIN` must be exactly the
`https://` names browsers use, and the proxy must pass the `Host` header through unchanged
(Caddy does): the dashboard answers only on its own host name.

## 2. First run

```sh
curl -fsS http://127.0.0.1:56080/readyz          # {"status":"ready", ... every task running}
curl -s -X POST https://api.sandbox.example.com/v1/projects \
  -H "Authorization: Bearer $PS_BOOTSTRAP_TOKEN" -H 'Content-Type: application/json' \
  -d '{"slug": "team-store", "name": "Team store"}'
```

The answer holds the project's first admin key, shown once. Give each developer a key of
their own (`POST /v1/keys`, scopes `read`, `payments:write` or `admin`) and check the whole
path with the CLI from any machine:

```sh
export PS_API_URL=https://api.sandbox.example.com PS_API_KEY=sk_sim_...
sandbox-cli --allow-non-local status
sandbox-cli --allow-non-local scenarios run --all --check
```

The CLI speaks HTTPS with rustls and trusts the machine's own certificate authorities, so a
certificate Caddy obtained from a public authority just works. If your proxy uses a private
authority (Caddy's `tls internal`, a company CA), give the CLI that authority's certificate:
`--ca-cert /path/to/root.crt` (or `PS_CA_CERT`). A certificate the CLI cannot trust is
reported as such (exit 5), and nothing is sent. CI runs this path: the release smoke puts
the API behind Caddy with its local authority and pays over `https` (D-074).

Sign in to `https://dashboard.sandbox.example.com` with a key. The checkout's pages are on
`https://pay.sandbox.example.com`.

## 3. Upgrade

1. Read the release's `docs/DECISIONS.md` entries since your version; a migration that
   changes the journal's protections says so.
2. Back up (`systemctl start payment-sandbox-backup`, section 4) and stop the server:
   `systemctl stop payment-sandbox-server`.
3. Install the new binaries and web builds as in section 1.
4. `systemctl start payment-sandbox-migrate`; it is idempotent, and it fails, changing
   nothing, if a migration cannot apply.
5. `systemctl restart payment-sandbox-sim && systemctl start payment-sandbox-server`. The
   server checks its role and the journal's protections at startup and refuses to serve a
   weakened database (D-058).
6. `curl -fsS http://127.0.0.1:56080/readyz` and
   `sandbox-cli --allow-non-local scenarios run --all --check`: every library scenario must
   still produce its committed trace.

Releases keep the API under `/v1`, every recorded journal entry's
`LEDGER_SEMANTICS_VERSION`, and the scenario library's traces; each migration is tested
against the previous release's data (D-072).

## 4. Backup and restore

The server's PostgreSQL database and the network's store (`PS_SIM_DB`) are separate
failure domains; `payment-sandbox-backup.timer` backs up both every night at about 03:17
(a missed run happens at the next boot), into `/var/backups/payment-sandbox` (root only):

- `db-<stamp>.dump`: a live `pg_dump --format=custom` as the schema owner. One consistent
  snapshot; the server keeps serving. Each dump is read back (`pg_restore --list`).
- `sim-<stamp>.redb`: the network's store, copied while the network is stopped for the
  copy (well under a second for a small store; a payment sent meanwhile comes back
  `uncertain` and the reconciler settles it).
- `etc-<stamp>.tar`: `/etc/payment-sandbox`, secrets included: `PS_SECRET_KEY` opens the
  stored webhook secrets, so it belongs with the dump.

The newest 14 of each are kept (`PS_BACKUP_KEEP` and `PS_BACKUP_DIR` in `backup.env`).
**Copy the directory off the host** (rsync, restic, your backup system): a backup on the same
disk is not a backup. `systemctl start payment-sandbox-backup` runs one now;
`journalctl -u payment-sandbox-backup` shows each run.

**Restore** into a fresh database created the way `setup` creates it (it keeps the
passwords), then put back the network's store from the same backup and let the server check
everything:

```sh
b=/var/backups/payment-sandbox; stamp=20261117T031700Z       # the backup to restore
systemctl stop payment-sandbox-server
sudo -u postgres env PS_ADMIN_DATABASE_URL='postgres:///postgres?host=/var/run/postgresql&user=postgres' \
  PS_DATABASE_NAME=payment_sandbox_restored /opt/payment-sandbox/bin/sandbox-server setup
pg_restore --single-transaction --exit-on-error --no-owner --role=ps_owner \
  --dbname="postgres://ps_owner:...@127.0.0.1:5432/payment_sandbox_restored" "$b/db-$stamp.dump"
# point PS_DATABASE_URL (server.env) and PS_MIGRATE_DATABASE_URL (migrate.env) at
# payment_sandbox_restored, then put the network's store back:
systemctl stop payment-sandbox-sim
install -o payment-sandbox -g payment-sandbox -m 0600 "$b/sim-$stamp.redb" /var/lib/payment-sandbox/sim.redb
systemctl start payment-sandbox-sim payment-sandbox-migrate payment-sandbox-server
curl -fsS http://127.0.0.1:56080/readyz
sandbox-cli reconcile --network                              # clean: the two agree
```

CI's release job runs exactly this on every push (D-083): a payment, the backup service
(the timer scheduled), then the restore into a fresh database with the store put back, the
payment found again, a clean `reconcile --network` and a new payment.
`scripts/restore-rehearsal.sh <source> <copy>` is the stricter check of the database part:
the same dump and single-transaction restore across two clusters, then a fingerprint
comparison and the protection checks on both sides (it needs a quiet source, so the
fingerprints can match).

Restoring the database from **before** some operations, with a newer network store, is
safe (drill c, D-015): `GET /v1/reconciliation?network=true` lists the operations the
network has and the server lost, and a client retrying under its original idempotency key
re-derives the same identity and records the network's answer once, never charging again.
Explanation cursors from before a restore are refused (`409 stale_cursor`); start the
explanation again.

## 5. Troubleshooting

| Symptom | Cause | Do |
|---|---|---|
| The server exits at startup naming a variable | a missing, empty or weak setting | fix that line of `server.env`; secrets need 32+ characters |
| `refusing to run: the database's protections were weakened: ...` | a trigger, function or grant differs from the migrations (D-058) | restore the migrations' state (re-run migrate on a fresh database and restore into it); never edit the journal's triggers by hand |
| `refusing to run as ...` | the server connects as the owner, a superuser or a role with TEMP (D-038) | `PS_DATABASE_URL` must use `ps_app` |
| `/readyz` 503 `reason: database` | PostgreSQL unreachable or slower than the readiness deadline | check `systemctl status postgresql`, the URL, the password |
| `/readyz` 503 `reason: background_task` | the reconciler or deliverer is down between restarts, or stuck | the journal shows its `last_failure`; it restarts with backoff by itself (D-051) |
| Operations stay `uncertain` | the network is unreachable, or `PS_SIM_TOKEN` differs between the two env files | `systemctl status payment-sandbox-sim`; the server logs `readback failed` with the reason |
| Webhook deliveries end `dead` | the receiver is not on this host, or its port is not in `PS_WEBHOOK_PORTS` | receivers must be loopback on listed ports (D-023, D-059); `sandbox-cli events` shows each attempt |
| The dashboard answers `421 misdirected` or redirects | the browser's host name is not `PS_DASHBOARD_ORIGIN`'s, or the proxy rewrote `Host` | make the origin the exact `https://` name; pass `Host` through |
| Signing in to the dashboard works, then every request is `403` | the page's `Origin` differs from `PS_DASHBOARD_ORIGIN` (a second name, `http` vs `https`) | use exactly one name |
| Everyone is signed out | the server restarted (sessions are in memory, D-062) | sign in again |
| `429 rate_limited` on reports | a project spent its report budget (D-047) | wait `Retry-After` seconds; raise `PS_REPORT_BURST` only with a measurement |
| `sandbox-cli` says `is not this machine` | the API is on the shared host | pass `--allow-non-local` (or `PS_ALLOW_NON_LOCAL=1`) |
| `sandbox-cli` says `http:// would send your sandbox key in clear text` | an `http://` URL for another machine | use the proxy's `https://` name |
| `sandbox-cli` says `could not open a secure (TLS) connection ... not trusted` | the proxy's certificate comes from an authority this machine does not trust | pass that authority's certificate with `--ca-cert` (or `PS_CA_CERT`) |
| `sandbox-cli` says `does not speak TLS on this port` / `the answer was not HTTP` | `https://` to a plain port, or `http://` to the proxy's TLS port | use the proxy's `https://` name, or `http://127.0.0.1:56080` on the host |

Logs: `journalctl -u payment-sandbox-server -u payment-sandbox-sim`. They carry operation
ids, codes and route templates, never keys, tokens or card-like input.
