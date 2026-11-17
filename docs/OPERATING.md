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
install -m 0755 target/release/sandbox-server target/release/sandbox-sim target/release/sandbox-cli /opt/payment-sandbox/bin/
cp -R apps/checkout/build /opt/payment-sandbox/checkout
cp -R apps/dashboard/build /opt/payment-sandbox/dashboard
install -d -m 0750 -g payment-sandbox /etc/payment-sandbox
for f in server sim migrate; do install -m 0640 -g payment-sandbox deploy/env/$f.env.example /etc/payment-sandbox/$f.env; done
install -m 0644 deploy/systemd/*.service /etc/systemd/system/
```

Fill in `/etc/payment-sandbox/*.env`. Each line is explained in the file; the empty secrets
must be generated (`openssl rand -hex 32` for each of `PS_SIM_TOKEN`, the same value in
`server.env` and `sim.env`, `PS_BOOTSTRAP_TOKEN` and `PS_SECRET_KEY`), and the database
passwords must be your own (16 to 128 letters, digits or `._~-`; `openssl rand -hex 24`).
A missing or weak value stops the process with a message naming it. `setup` never resets
an existing role's password: running it again (for a restore) is safe.

Create the roles and the database (once, as PostgreSQL's administrator) with passwords of
your own, the same ones `migrate.env` and `server.env` use, then migrate as the owner:

```sh
PS_ADMIN_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres \
PS_OWNER_PASSWORD=your-owner-password PS_APP_PASSWORD=your-app-password \
  /opt/payment-sandbox/bin/sandbox-server setup
systemctl daemon-reload
systemctl start payment-sandbox-migrate
systemctl enable --now payment-sandbox-sim payment-sandbox-server
```

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
2. Back up (section 4) and stop the server: `systemctl stop payment-sandbox-server`.
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
failure domains, backed up separately; keep `PS_SECRET_KEY` with the database backup.

**Back up** while the server is stopped or idle (the rehearsal compares fingerprints, so a
write during the dump fails it, D-053):

```sh
systemctl stop payment-sandbox-server
pg_dump --format=custom --file=/backup/payment_sandbox.dump "postgres://ps_owner:...@127.0.0.1:5432/payment_sandbox"
systemctl stop payment-sandbox-sim && cp /var/lib/payment-sandbox/sim.redb /backup/sim.redb && systemctl start payment-sandbox-sim
systemctl start payment-sandbox-server
```

**Restore** into a fresh database created the way `setup` creates it, then let the server
check it:

```sh
systemctl stop payment-sandbox-server
PS_ADMIN_DATABASE_URL=... PS_DATABASE_NAME=payment_sandbox_restored /opt/payment-sandbox/bin/sandbox-server setup   # keeps the passwords
pg_restore --single-transaction --exit-on-error --no-owner --role=ps_owner \
  --dbname="postgres://ps_owner:...@127.0.0.1:5432/payment_sandbox_restored" /backup/payment_sandbox.dump
# point PS_DATABASE_URL and PS_MIGRATE_DATABASE_URL at payment_sandbox_restored, then:
systemctl start payment-sandbox-migrate payment-sandbox-server
```

`scripts/restore-rehearsal.sh <source> <copy>` is the tested form of this procedure (CI
runs it): the same dump and single-transaction restore, run as PostgreSQL's administrator
inside its container, then a fingerprint comparison and the protection checks on both
sides. The commands above, as the owner on a host, are not run by CI; rehearse them on a
copy before you need them.

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
| `sandbox-cli` says `could not open a secure (TLS) connection ... not trusted` | the proxy's certificate comes from an authority this machine does not trust | pass that authority's certificate with `--ca-cert` (or `PS_CA_CERT`) |
| `sandbox-cli` says `does not speak TLS on this port` / `the answer was not HTTP` | `https://` to a plain port, or `http://` to the proxy's TLS port | use the proxy's `https://` name, or `http://127.0.0.1:56080` on the host |

Logs: `journalctl -u payment-sandbox-server -u payment-sandbox-sim`. They carry operation
ids, codes and route templates, never keys, tokens or card-like input.
