-- Payment Sandbox, migration 14: derived per-account balances (D-096). SIMULATED MONEY
-- ONLY.
--
-- /v1/balances and each explain page summed every journal line of the epoch on each call
-- (the m5-explain full run: PostgreSQL spent ~2 cores on repeated scans). Balances are now
-- kept per (project, epoch, currency, account) in the SAME statement that posts the
-- entry's lines, so a balance can never exist without its lines nor disagree with them at
-- commit. The journal stays the only authority: this table is a derivation, its rows are
-- recreated by the backfill below whenever needed, and reconciliation re-derives it from
-- the journal and reports every difference (a planted drift is detected).

CREATE TABLE account_balances (
    project_id uuid NOT NULL,
    dataset_epoch integer NOT NULL,
    currency text NOT NULL,
    account_kind text NOT NULL,
    -- Which of 16 stripes carries the entry's delta (hash of the entry id). One row per
    -- account would make every posting of a project wait on the same row locks until
    -- commit — measured ~7x slower at 64 concurrent postings, flat past 8. The balance
    -- is the sum over its stripes; reads stay O(accounts x stripes) = O(accounts x 16).
    stripe smallint NOT NULL CHECK (stripe BETWEEN 0 AND 15),
    -- Signed minor units: debit positive, like the lines it sums. Deliberately no bound
    -- narrower than the journal's: this column must be able to hold anything the lines sum
    -- to, and an impossible sum is a statement error, not a silent clamp.
    balance_minor bigint NOT NULL,
    -- Lines summed so far; the explain page's horizon count is derived from it too.
    line_count bigint NOT NULL CHECK (line_count > 0),
    PRIMARY KEY (project_id, dataset_epoch, currency, account_kind, stripe),
    FOREIGN KEY (project_id, currency, account_kind) REFERENCES accounts (project_id, currency, kind)
);

-- Backfill existing databases: one set pass over the journal, exactly the sums the old
-- read path computed, all on stripe 0 (reads sum the stripes either way).
INSERT INTO account_balances (project_id, dataset_epoch, currency, account_kind, stripe,
                              balance_minor, line_count)
SELECT project_id, dataset_epoch, currency, account_kind, 0,
       sum(amount_minor)::bigint, count(*)::bigint
  FROM journal_lines
 GROUP BY project_id, dataset_epoch, currency, account_kind;

-- The posting upsert is insert-or-update; reads are the balances and explain paths.
GRANT SELECT, INSERT, UPDATE ON account_balances TO ps_runtime;
