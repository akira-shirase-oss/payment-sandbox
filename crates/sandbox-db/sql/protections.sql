-- The journal's protections, stated once (review N5; D-058). SIMULATED MONEY ONLY.
--
-- One row per problem; no rows means the database is protected as the migrations left it.
-- A migration that changes a trigger or a function must update its digest here in the same
-- commit (`md5(pg_get_triggerdef(oid))`, `md5(pg_get_functiondef(oid))` on a fresh database).
-- Checked absolutely, not by comparison with another copy: the restore rehearsal runs it on
-- the source and on the copy (scripts/restore-rehearsal.sh), and the server runs it at
-- startup and refuses to serve when it returns anything (sandbox_db::roles::check_runtime).
-- Every trigger below must exist, be enabled and keep its exact definition; every function
-- in `public` must be one listed, keep its exact definition (body included), pin its search
-- path, be SECURITY INVOKER and belong to ps_owner (a second review check: names alone let
-- a no-op body through); `ps_runtime` must hold exactly the listed table privileges, may not create
-- temporary tables or objects in `public`, and `ps_owner` must own every table.
WITH expected_triggers (tbl, name, digest) AS (
    -- md5(pg_get_triggerdef(oid)): the events, timing, level, deferral and function.
    VALUES ('audit_events', 'audit_events_append_only', '840ce02299857c3b0d6f322dc8ec6dd8'),
           ('audit_events', 'audit_events_no_truncate', '0a7c918edf788926b7af1cf537e8edfb'),
           ('events', 'events_append_only', 'fcc09665686ff23b157bbbc542baf67f'),
           ('events', 'events_no_truncate', '2e7a1229fe028261672001f3e492f451'),
           ('journal_entries', 'journal_entries_append_only', 'df1436dcbf9e28d88f587f2f80511f9c'),
           ('journal_entries', 'journal_entries_have_lines', '704d41c83f8fbe45cfbed4a9407fdde3'),
           ('journal_entries', 'journal_entries_no_truncate', 'c8dff5913a4cd99048a854d05f116f24'),
           ('journal_entries', 'journal_entries_stamp', '6a2eaf82dbc554bd6e4ee10c8dfaf3b9'),
           ('journal_lines', 'journal_lines_append_only', '4978365e3afc48a4e41284d248455d4d'),
           ('journal_lines', 'journal_lines_balanced', 'fc42d40baa4171ca3cf5b12e65819ea3'),
           ('journal_lines', 'journal_lines_no_truncate', 'b4f1af17dbc6a854ac9b6f5d5c9efd2d'),
           ('operations', 'operations_no_delete', 'c86329765441658d9bf6e22ecdf897a5'),
           ('operations', 'operations_no_truncate', 'fca21370b8e532d970738903402625db'),
           ('operations', 'operations_resolved_is_final', 'f5ab60868728ffeae03a024304f87cc3'),
           ('payments', 'payments_no_delete', '9d8be5096e435ecdfe4e228c330ab2c1'),
           ('payments', 'payments_no_truncate', 'e78203e1076c56708807bba773ee3dc9'),
           ('projects', 'projects_epoch_moves_forward', '42f3bb0af470fada5913435d2478ec18'),
           ('projects', 'projects_no_delete', 'b53bfe3a62d4ecd4979b8f59c38e1056'),
           ('projects', 'projects_no_truncate', '2ab4b2e96a7e082b0e910c5df0d04bf9')
), expected_functions (name, digest) AS (
    -- md5(pg_get_functiondef(oid)): the whole definition, body and settings included.
    VALUES ('journal_entries_check_lines', '0751af9ed7f638e9d937e6b5556d19a7'),
           ('journal_entries_stamp', '8df55a68832985280d17f363674717c8'),
           ('journal_lines_check_balance', '45d580297b8266c2c2d8ad823546e34b'),
           ('journal_refuse_change', '92238c34856bc8af8688e3c80df79b27'),
           ('operations_resolved_is_final', 'ad4f3d10a4cc76f979df1abac1648a39'),
           ('projects_epoch_moves_forward', 'f419c2cbb6002139fafd2d24eb5aee23')
), expected_grants (tbl, privileges) AS (
    VALUES ('accounts', 'INSERT,SELECT'),
           ('api_keys', 'INSERT,SELECT,UPDATE'),
           ('audit_events', 'INSERT,SELECT'),
           ('checkout_attempts', 'INSERT,SELECT'),
           ('checkout_sessions', 'INSERT,SELECT,UPDATE'),
           ('deliveries', 'INSERT,SELECT,UPDATE'),
           ('events', 'INSERT,SELECT'),
           ('journal_entries', 'INSERT,SELECT'),
           ('journal_lines', 'INSERT,SELECT'),
           ('operations', 'INSERT,SELECT,UPDATE'),
           ('payments', 'INSERT,SELECT,UPDATE'),
           ('projects', 'INSERT,SELECT'),
           ('webhook_endpoints', 'INSERT,SELECT,UPDATE'),
           ('_sqlx_migrations', '')
), expected_column_grants (tbl, col, privileges) AS (
    -- Privileges held on single columns only, beyond the table-level ones above (D-066):
    -- a reset writes this one column (and locks the row) and nothing else of a project.
    VALUES ('projects', 'dataset_epoch', 'UPDATE')
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
), column_held AS (
    -- A column grant (GRANT UPDATE (col) ON t) is invisible to has_table_privilege, so a
    -- weakening such as UPDATE on journal_lines.amount_minor passed the table check above.
    SELECT t.relname, a.attname,
           string_agg(x, ',' ORDER BY x) AS privileges
      FROM tables t
      JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum > 0 AND NOT a.attisdropped
      CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'REFERENCES']) AS x
     WHERE pg_catalog.has_column_privilege('ps_runtime', t.oid, a.attnum, x)
       AND NOT pg_catalog.has_table_privilege('ps_runtime', t.oid, x)
     GROUP BY t.relname, a.attname
)
SELECT problem FROM (
    SELECT format('trigger %s.%s is missing or not enabled', e.tbl, e.name) AS problem
      FROM expected_triggers e
     WHERE NOT EXISTS (
         SELECT 1 FROM pg_catalog.pg_trigger t
          WHERE t.tgrelid = pg_catalog.to_regclass('public.' || e.tbl)
            AND t.tgname = e.name AND t.tgenabled = 'O' AND NOT t.tgisinternal)
    UNION ALL
    SELECT format('trigger %s.%s is not defined as the migrations left it', e.tbl, e.name)
      FROM expected_triggers e
      JOIN pg_catalog.pg_trigger t
        ON t.tgrelid = pg_catalog.to_regclass('public.' || e.tbl) AND t.tgname = e.name
     WHERE pg_catalog.md5(pg_catalog.pg_get_triggerdef(t.oid)) <> e.digest
    UNION ALL
    SELECT format('function %s is missing', e.name)
      FROM expected_functions e
     WHERE NOT EXISTS (
         SELECT 1 FROM pg_catalog.pg_proc p
          JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = 'public' AND p.proname = e.name)
    UNION ALL
    SELECT format('function %s is not defined as the migrations left it', p.proname)
      FROM pg_catalog.pg_proc p
      JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
      LEFT JOIN expected_functions e ON e.name = p.proname
     WHERE n.nspname = 'public'
       AND pg_catalog.md5(pg_catalog.pg_get_functiondef(p.oid)) IS DISTINCT FROM e.digest
    UNION ALL
    SELECT format('function %s is SECURITY DEFINER or not owned by ps_owner', p.proname)
      FROM pg_catalog.pg_proc p
      JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
     WHERE n.nspname = 'public'
       AND (p.prosecdef OR pg_catalog.pg_get_userbyid(p.proowner) <> 'ps_owner')
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
    SELECT format('ps_runtime holds [%s] on column %s.%s; expected [%s]', c.privileges, c.relname,
                  c.attname, coalesce(g.privileges, ''))
      FROM column_held c
      LEFT JOIN expected_column_grants g ON g.tbl = c.relname AND g.col = c.attname
     WHERE c.privileges <> coalesce(g.privileges, '')
    UNION ALL
    SELECT format('ps_runtime lacks [%s] on column %s.%s', g.privileges, g.tbl, g.col)
      FROM expected_column_grants g
     WHERE NOT EXISTS (SELECT 1 FROM column_held c
                        WHERE c.relname = g.tbl AND c.attname = g.col AND c.privileges = g.privileges)
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
