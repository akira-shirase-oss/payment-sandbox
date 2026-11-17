-- Payment Sandbox, migration 6: the journal's trigger functions cannot be fooled by a
-- temporary table (review finding 2; D-039). SIMULATED MONEY ONLY.
--
-- PostgreSQL searches the session's pg_temp schema *first* for relations unless the path
-- lists it explicitly. The functions below name every relation with its schema, and every
-- function pins `search_path = pg_catalog, pg_temp` (pg_temp last), so neither a shadow
-- table nor a shadow function can answer their queries.

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
     GROUP BY e.project_id, e.id, e.created_xact
    HAVING sum(l.amount_minor) <> 0
        OR count(*) < 2
        OR e.created_xact <> pg_catalog.pg_current_xact_id()
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'journal entry % is unbalanced, has fewer than two lines, or was posted earlier', offending
            USING ERRCODE = 'PSJ02';
    END IF;
    RETURN NULL;
END
$$;

CREATE OR REPLACE FUNCTION public.journal_entries_check_lines() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    line_count bigint;
    total numeric;
BEGIN
    SELECT count(*), coalesce(sum(amount_minor), 0) INTO line_count, total
      FROM public.journal_lines
     WHERE project_id = NEW.project_id AND entry_id = NEW.id;
    IF line_count < 2 OR total <> 0 THEN
        RAISE EXCEPTION 'journal entry % has no balanced lines at commit', NEW.id
            USING ERRCODE = 'PSJ03';
    END IF;
    RETURN NULL;
END
$$;

CREATE OR REPLACE FUNCTION public.journal_entries_stamp() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    NEW.posted_at := pg_catalog.now();
    NEW.created_xact := pg_catalog.pg_current_xact_id();
    RETURN NEW;
END
$$;

ALTER FUNCTION public.journal_refuse_change() SET search_path = pg_catalog, pg_temp;
ALTER FUNCTION public.operations_resolved_is_final() SET search_path = pg_catalog, pg_temp;
