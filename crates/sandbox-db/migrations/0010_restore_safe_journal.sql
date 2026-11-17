-- Payment Sandbox, migration 10: journal rules that survive a restore into another cluster
-- (review N2; D-055). SIMULATED MONEY ONLY.
--
-- `created_xact` is the id of the transaction that posted an entry, which means something
-- only in the cluster that assigned it. A logical restore copies it into a cluster whose
-- counter is unrelated, where it may be "in the future" (so a snapshot filter hid every
-- restored entry) or, later, equal to a live transaction's id (so the late-line rule let
-- that transaction add lines to a posted entry).
--
-- The row's own `xmin` always belongs to the current cluster: a restore inserts every row
-- anew. An entry is *native* when its `xmin` is the posting transaction's id
-- (`xmin = created_xact::xid`, what the stamp trigger guarantees for every entry posted
-- here, outside savepoints). A restored entry is not native: it was committed before any
-- snapshot of this cluster, and no transaction of this cluster posted it. So:
--   * a line may join an entry only if the entry is native *and* posted by this very
--     transaction (both checks, never the stored id alone);
--   * explanations treat a non-native entry as committed (reconcile.rs).

CREATE OR REPLACE FUNCTION public.journal_lines_check_balance() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    offending uuid;
BEGIN
    SELECT e.id INTO offending
      FROM (SELECT DISTINCT project_id, entry_id FROM new_lines) touched
      JOIN public.journal_entries e ON e.project_id = touched.project_id AND e.id = touched.entry_id
      JOIN public.journal_lines l ON l.project_id = touched.project_id AND l.entry_id = touched.entry_id
     GROUP BY e.project_id, e.id, e.created_xact, e.xmin
    HAVING sum(l.amount_minor) <> 0
        OR count(*) < 2
        OR e.created_xact <> pg_catalog.pg_current_xact_id()
        OR e.xmin <> e.created_xact::xid
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'journal entry % is unbalanced, has fewer than two lines, or was posted earlier', offending
            USING ERRCODE = 'PSJ02';
    END IF;
    RETURN NULL;
END
$$;
