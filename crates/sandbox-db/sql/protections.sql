-- The journal's protections, stated once (review N5; D-058). SIMULATED MONEY ONLY.
--
-- One row per problem; no rows means the database is protected as the migrations left it.
-- Checked absolutely, not by comparison with another copy: the restore rehearsal runs it on
-- the source and on the copy (scripts/restore-rehearsal.sh), and the server runs it at
-- startup and refuses to serve when it returns anything (sandbox_db::roles::check_runtime).
-- Every trigger below must exist and be enabled; every function in `public` must pin its
-- search path; `ps_runtime` must hold exactly the listed table privileges, may not create
-- temporary tables or objects in `public`, and `ps_owner` must own every table.
WITH expected_triggers (tbl, name) AS (
    VALUES ('audit_events', 'audit_events_append_only'),
           ('audit_events', 'audit_events_no_truncate'),
           ('events', 'events_append_only'),
           ('events', 'events_no_truncate'),
           ('journal_entries', 'journal_entries_append_only'),
           ('journal_entries', 'journal_entries_have_lines'),
           ('journal_entries', 'journal_entries_no_truncate'),
           ('journal_entries', 'journal_entries_stamp'),
           ('journal_lines', 'journal_lines_append_only'),
           ('journal_lines', 'journal_lines_balanced'),
           ('journal_lines', 'journal_lines_no_truncate'),
           ('operations', 'operations_no_delete'),
           ('operations', 'operations_no_truncate'),
           ('operations', 'operations_resolved_is_final'),
           ('payments', 'payments_no_delete'),
           ('payments', 'payments_no_truncate')
), expected_functions (name) AS (
    VALUES ('journal_entries_check_lines'), ('journal_entries_stamp'),
           ('journal_lines_check_balance'), ('journal_refuse_change'),
           ('operations_resolved_is_final')
), expected_grants (tbl, privileges) AS (
    VALUES ('accounts', 'INSERT,SELECT'),
           ('api_keys', 'INSERT,SELECT,UPDATE'),
           ('audit_events', 'INSERT,SELECT'),
           ('deliveries', 'INSERT,SELECT,UPDATE'),
           ('events', 'INSERT,SELECT'),
           ('journal_entries', 'INSERT,SELECT'),
           ('journal_lines', 'INSERT,SELECT'),
           ('operations', 'INSERT,SELECT,UPDATE'),
           ('payments', 'INSERT,SELECT,UPDATE'),
           ('projects', 'INSERT,SELECT'),
           ('webhook_endpoints', 'INSERT,SELECT,UPDATE'),
           ('_sqlx_migrations', '')
), tables AS (
    SELECT c.oid, c.relname, pg_catalog.pg_get_userbyid(c.relowner) AS owner
      FROM pg_catalog.pg_class c
      JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
), held AS (
    SELECT t.relname,
           coalesce(string_agg(p.privilege, ',' ORDER BY p.privilege), '') AS privileges
      FROM tables t
      LEFT JOIN LATERAL (
          SELECT x AS privilege
            FROM unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER']) AS x
           WHERE pg_catalog.has_table_privilege('ps_runtime', t.oid, x)
      ) p ON true
     GROUP BY t.relname
)
SELECT problem FROM (
    SELECT format('trigger %s.%s is missing or not enabled', e.tbl, e.name) AS problem
      FROM expected_triggers e
     WHERE NOT EXISTS (
         SELECT 1 FROM pg_catalog.pg_trigger t
          WHERE t.tgrelid = pg_catalog.to_regclass('public.' || e.tbl)
            AND t.tgname = e.name AND t.tgenabled = 'O' AND NOT t.tgisinternal)
    UNION ALL
    SELECT format('function %s is missing', e.name)
      FROM expected_functions e
     WHERE NOT EXISTS (
         SELECT 1 FROM pg_catalog.pg_proc p
          JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = 'public' AND p.proname = e.name)
    UNION ALL
    SELECT format('function %s does not pin search_path = pg_catalog, pg_temp', p.proname)
      FROM pg_catalog.pg_proc p
      JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND NOT coalesce(p.proconfig @> ARRAY['search_path=pg_catalog, pg_temp'], false)
    UNION ALL
    SELECT format('ps_runtime holds [%s] on %s; expected [%s]', h.privileges, h.relname,
                  coalesce(g.privileges, ''))
      FROM held h LEFT JOIN expected_grants g ON g.tbl = h.relname
     WHERE h.privileges <> coalesce(g.privileges, '')
    UNION ALL
    SELECT format('table %s is missing', g.tbl)
      FROM expected_grants g
     WHERE NOT EXISTS (SELECT 1 FROM tables t WHERE t.relname = g.tbl)
    UNION ALL
    SELECT format('table %s is owned by %s, not ps_owner', t.relname, t.owner)
      FROM tables t WHERE t.owner <> 'ps_owner'
    UNION ALL
    SELECT 'ps_runtime may create temporary tables'
     WHERE pg_catalog.has_database_privilege('ps_runtime', pg_catalog.current_database(), 'TEMP')
    UNION ALL
    SELECT 'ps_runtime may create objects in schema public'
     WHERE pg_catalog.has_schema_privilege('ps_runtime', 'public', 'CREATE')
) problems
ORDER BY problem;
