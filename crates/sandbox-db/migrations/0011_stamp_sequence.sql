-- Payment Sandbox, migration 11: the database chooses every entry's sequence number
-- (review N8; D-059). SIMULATED MONEY ONLY.
--
-- `seq` is GENERATED ALWAYS AS IDENTITY, but INSERT ... OVERRIDING SYSTEM VALUE still lets
-- a caller choose it: the runtime role stored an entry at seq = -5, which explain (paging
-- seq > after_seq from 0) would never list though it counts in the balance. The stamp
-- trigger now takes the sequence's next value whatever the caller wrote, as it already does
-- for posted_at and created_xact. (An insert that does not override consumes two values;
-- sequence numbers are allocation order, never contiguous.)

CREATE OR REPLACE FUNCTION public.journal_entries_stamp() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    NEW.posted_at := pg_catalog.now();
    NEW.created_xact := pg_catalog.pg_current_xact_id();
    NEW.seq := pg_catalog.nextval(pg_catalog.pg_get_serial_sequence('public.journal_entries', 'seq'));
    RETURN NEW;
END
$$;
