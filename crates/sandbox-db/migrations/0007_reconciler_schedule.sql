-- Payment Sandbox, migration 7: the reconciler's schedule (review finding 11; D-048).
-- SIMULATED MONEY ONLY.
--
-- An open operation is due for a check at `next_check_at` ('-infinity': at once, on any
-- clock). A failed check (the network unreachable, an unusable answer) moves it forward
-- with exponential backoff and counts in `checks`, so an operation that keeps failing no
-- longer heads every sweep. The index serves the per-project round robin: the projects
-- with open operations, then each project's due operations in order.

ALTER TABLE operations
    ADD COLUMN next_check_at timestamptz NOT NULL DEFAULT '-infinity',
    ADD COLUMN checks integer NOT NULL DEFAULT 0 CHECK (checks >= 0);

CREATE INDEX operations_open_due ON operations (project_id, next_check_at, created_at)
    WHERE state IN ('pending', 'uncertain');
