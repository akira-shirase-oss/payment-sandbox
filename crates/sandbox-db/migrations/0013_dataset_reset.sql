-- Payment Sandbox, migration 13: a dataset reset opens a new epoch (D-022, D-066).
-- SIMULATED MONEY ONLY.
--
-- A reset moves a project's dataset_epoch forward by exactly one. Nothing of an earlier
-- epoch is deleted or changed: the journal, the events and the audit trail stay
-- append-only, payments and operations keep their own epoch and their own rules. The
-- runtime role may update that one column and nothing else of a project, and the trigger
-- below refuses every other change, for every role that has not switched triggers off
-- (which the protections manifest, sql/protections.sql, would report).

CREATE FUNCTION public.projects_epoch_moves_forward() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.slug IS DISTINCT FROM OLD.slug
       OR NEW.name IS DISTINCT FROM OLD.name OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.dataset_epoch IS DISTINCT FROM OLD.dataset_epoch + 1 THEN
        RAISE EXCEPTION 'project %: only a reset changes a project, moving its dataset epoch forward by one', OLD.id
            USING ERRCODE = 'PSP01';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER projects_epoch_moves_forward
    BEFORE UPDATE ON public.projects
    FOR EACH ROW EXECUTE FUNCTION public.projects_epoch_moves_forward();
-- A project, like its history, is never deleted.
CREATE TRIGGER projects_no_delete BEFORE DELETE ON public.projects
    FOR EACH ROW EXECUTE FUNCTION public.journal_refuse_change();
CREATE TRIGGER projects_no_truncate BEFORE TRUNCATE ON public.projects
    FOR EACH STATEMENT EXECUTE FUNCTION public.journal_refuse_change();

-- The one column a reset writes (and the row lock it takes, SELECT ... FOR UPDATE).
GRANT UPDATE (dataset_epoch) ON public.projects TO ps_runtime;
