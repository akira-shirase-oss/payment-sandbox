#!/usr/bin/env bash
# The logical-restore rehearsal (D-026, D-053): back up a database with pg_dump, restore it
# into a fresh database, and prove the copy is the same ledger with the same protections.
#
#   scripts/restore-rehearsal.sh <source database> <new database>
#
# 1. pg_dump --format=custom of the source (a consistent snapshot; the server may keep
#    running);
# 2. a fresh database made the way `sandbox-server setup` makes one (owner ps_owner, only
#    ps_runtime may connect, nobody may create temporary tables);
# 3. pg_restore in one transaction, stopping at the first error;
# 4. a fingerprint of both databases must be identical: every table's row count, a digest
#    of every journal entry and line, every trigger (enabled) and its function, every
#    function's pinned search_path and owner, every object's owner, the runtime role's
#    grants, and every sequence's position;
# 5. invariants on the copy: every entry balances per currency, no entry lacks lines, no
#    trigger is disabled, the journal has its triggers, and the runtime role can neither
#    TRUNCATE nor create temporary tables.
#
# pg_dump, pg_restore and psql run inside the PostgreSQL containers, as their superuser, so
# their version always matches the server's: PS_PG_CONTAINER holds the source (default the
# local stack's payment-sandbox-postgres-1; CI passes its service container) and
# PS_PG_TARGET_CONTAINER the copy (default the same cluster; another container restores
# into another cluster, D-055, whose roles `sandbox-server setup` must have created first);
# PS_PG_SUPERUSER (default sandbox). The new database must not exist. Payment sums against the journal and
# the network are then checked by the reconciliation report (tests/restore.rs runs it).
set -euo pipefail

usage="usage: scripts/restore-rehearsal.sh <source database> <new database>"
src="${1:?$usage}"
dst="${2:?$usage}"
for name in "$src" "$dst"; do
  [[ "$name" =~ ^[a-z0-9_]{1,63}$ ]] || { echo "error: database names are [a-z0-9_]: $name" >&2; exit 2; }
done
[ "$src" != "$dst" ] || { echo "error: the copy needs a new name" >&2; exit 2; }
container="${PS_PG_CONTAINER:-payment-sandbox-postgres-1}"
target="${PS_PG_TARGET_CONTAINER:-$container}"
su="${PS_PG_SUPERUSER:-sandbox}"

in_pg() { docker exec -i "$container" "$@"; }
in_target() { docker exec -i "$target" "$@"; }
sql() {
  local db="$1"
  shift
  in_pg psql -X -q -v ON_ERROR_STOP=1 -At -U "$su" -d "$db" "$@"
}
sql_target() {
  local db="$1"
  shift
  in_target psql -X -q -v ON_ERROR_STOP=1 -At -U "$su" -d "$db" "$@"
}

dump="/tmp/ps-rehearsal-$$-${src}.dump"
trap 'in_pg rm -f "$dump" >/dev/null 2>&1 || true; in_target rm -f "$dump" >/dev/null 2>&1 || true' EXIT

[ "$(sql_target postgres -c "SELECT count(*) FROM pg_roles WHERE rolname IN ('ps_owner', 'ps_runtime', 'ps_app')")" = 3 ] \
  || { echo "error: the target cluster lacks the roles; run sandbox-server setup against it first" >&2; exit 2; }
in_pg pg_dump -U "$su" --format=custom --file="$dump" "$src"
if [ "$target" != "$container" ]; then
  in_pg cat "$dump" | in_target sh -c "cat > '$dump'"
fi
sql_target postgres -c "CREATE DATABASE $dst OWNER ps_owner"
sql_target postgres -c "REVOKE ALL ON DATABASE $dst FROM PUBLIC" -c "GRANT CONNECT ON DATABASE $dst TO ps_runtime"
in_target pg_restore -U "$su" --exit-on-error --single-transaction --dbname="$dst" "$dump"

fingerprint=$(cat <<'SQL'
SELECT k || ' = ' || v FROM (
  SELECT 'rows ' || c.relname AS k,
         (xpath('/row/n/text()',
                query_to_xml(format('SELECT count(*) AS n FROM public.%I', c.relname), false, true, '')))[1]::text AS v
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND c.relkind = 'r'
  UNION ALL
  SELECT 'digest journal_entries',
         coalesce(md5(string_agg(concat_ws('|', project_id, id, seq, dataset_epoch, currency, recipe,
                  semantics_version, operation_id, payment_id, reverses_entry_id, posted_at, created_xact),
                  ',' ORDER BY seq)), '-')
    FROM public.journal_entries
  UNION ALL
  SELECT 'digest journal_lines',
         coalesce(md5(string_agg(concat_ws('|', project_id, entry_id, line_no, currency, dataset_epoch,
                  account_kind, amount_minor), ',' ORDER BY project_id, entry_id, line_no)), '-')
    FROM public.journal_lines
  UNION ALL
  SELECT 'digest operations',
         coalesce(md5(string_agg(concat_ws('|', project_id, id, kind, amount_minor, state, decline_code,
                  network_ref), ',' ORDER BY project_id, id)), '-')
    FROM public.operations
  UNION ALL
  SELECT 'trigger ' || tgrelid::regclass || '.' || tgname, tgenabled::text || ' ' || tgfoid::regproc::text
    FROM pg_trigger WHERE NOT tgisinternal
  UNION ALL
  SELECT 'function ' || p.oid::regprocedure::text,
         coalesce(array_to_string(p.proconfig, ','), '-') || ' owner ' || pg_get_userbyid(p.proowner)
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public'
  UNION ALL
  SELECT 'owner ' || c.relname, pg_get_userbyid(c.relowner)
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'public' AND c.relkind IN ('r', 'S', 'i')
  UNION ALL
  SELECT 'runtime grants ' || c.relname,
         concat_ws(',', p.privs)
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   CROSS JOIN LATERAL (
       SELECT string_agg(x, ',' ORDER BY x) AS privs
         FROM unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE']) AS x
        WHERE has_table_privilege('ps_runtime', c.oid, x)) p
   WHERE n.nspname = 'public' AND c.relkind = 'r'
  UNION ALL
  SELECT 'sequence ' || sequencename, coalesce(last_value::text, '-')
    FROM pg_sequences WHERE schemaname = 'public'
) f ORDER BY 1;
SQL
)

before="$(sql "$src" -c "$fingerprint")"
after="$(sql_target "$dst" -c "$fingerprint")"
if [ "$before" != "$after" ]; then
  echo "error: the restored copy differs from its source:" >&2
  diff <(printf '%s\n' "$before") <(printf '%s\n' "$after") >&2 || true
  exit 1
fi

invariants=$(cat <<'SQL'
SELECT concat_ws(' ',
  'unbalanced=' || (SELECT count(*) FROM (SELECT 1 FROM public.journal_lines
                     GROUP BY project_id, entry_id, currency HAVING sum(amount_minor) <> 0) u),
  'entries_without_lines=' || (SELECT count(*) FROM public.journal_entries e WHERE NOT EXISTS (
                     SELECT 1 FROM public.journal_lines l WHERE l.project_id = e.project_id AND l.entry_id = e.id)),
  'disabled_triggers=' || (SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal AND tgenabled <> 'O'),
  'runtime_truncate=' || (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE n.nspname = 'public' AND c.relkind = 'r'
                       AND has_table_privilege('ps_runtime', c.oid, 'TRUNCATE')),
  'runtime_temp=' || has_database_privilege('ps_runtime', current_database(), 'TEMP')::int,
  'journal_triggers=' || (SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal
                     AND tgrelid IN ('public.journal_entries'::regclass, 'public.journal_lines'::regclass)));
SQL
)
checked="$(sql_target "$dst" -c "$invariants")"
for zero in unbalanced entries_without_lines disabled_triggers runtime_truncate runtime_temp; do
  if ! grep -qw "${zero}=0" <<<"$checked"; then
    echo "error: invariant broken on the copy: $checked" >&2
    exit 1
  fi
done
if grep -qw 'journal_triggers=0' <<<"$checked"; then
  echo "error: the copy's journal has no triggers: $checked" >&2
  exit 1
fi
echo "restore ok: $src -> $dst; $(wc -l <<<"$after" | tr -d ' ') fingerprint lines identical; $checked"
