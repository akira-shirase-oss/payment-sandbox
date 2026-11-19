-- D-098: the explain horizon's invisible-tail scanned every entry of the epoch per call
-- (a parallel seq scan, ~9 ms at 100k entries, O(entries) per read). An entry invisible to
-- a snapshot must have `created_xact >= pg_snapshot_xmin(snapshot)` — a strict superset of
-- the invisible set, always a small recent tail. This index makes that bound a range scan.
CREATE INDEX journal_entries_by_xact
    ON journal_entries (project_id, created_xact);
