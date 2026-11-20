-- Payment Sandbox, migration 16: cross-process auth invalidation (D-102's addendum).
-- SIMULATED MONEY ONLY.
--
-- The server's key-identity cache invalidates exactly for what ITS process changes;
-- a revocation by another server process, a CLI, or a psql would only be caught by the
-- 30 s TTL. These triggers make the database itself broadcast the change on the
-- `auth_epoch` channel: payload `k<project_id>:<key_id>` when a key is revoked,
-- `p<project_id>` when a dataset epoch opens. Each server LISTENs and drops the exact
-- entry — so a revoked key fails on its next request in every process, and the TTL is
-- only the missed-notification backstop (a listener that lost events flushes the whole
-- cache on reconnect).

CREATE FUNCTION public.auth_epoch_notify() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_TABLE_NAME = 'api_keys' THEN
        PERFORM pg_notify('auth_epoch', 'k' || NEW.project_id::text || ':' || NEW.id::text);
    ELSE
        PERFORM pg_notify('auth_epoch', 'p' || NEW.id::text);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER api_keys_auth_epoch
    AFTER UPDATE OF revoked_at ON public.api_keys
    FOR EACH ROW
    WHEN (OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL)
    EXECUTE FUNCTION public.auth_epoch_notify();

CREATE TRIGGER projects_auth_epoch
    AFTER UPDATE OF dataset_epoch ON public.projects
    FOR EACH ROW
    WHEN (OLD.dataset_epoch <> NEW.dataset_epoch)
    EXECUTE FUNCTION public.auth_epoch_notify();
