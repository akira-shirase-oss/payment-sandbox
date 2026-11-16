-- Payment Sandbox, migration 1: projects, the chart of accounts and the journal.
-- SIMULATED MONEY ONLY.
--
-- The journal is enforced here, not by convention (D-003):
--   * journal_entries and journal_lines refuse UPDATE, DELETE and TRUNCATE;
--   * every INSERT statement into journal_lines must leave each touched entry balanced
--     (signed lines sum to zero, at least two lines) and may only touch entries created in
--     the same transaction, so a committed entry can never gain lines;
--   * at commit, every new entry must have its lines;
--   * a line's currency and epoch must equal its entry's (composite foreign key), and its
--     account must be in the project's chart;
--   * posted_at and created_xact come from the database, never from the client, so no
--     application clock (virtual or wall) can backdate history.
-- Custom SQLSTATEs: PSJ01 append-only violation, PSJ02 unbalanced or foreign lines,
-- PSJ03 entry without lines at commit.

CREATE TABLE projects (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    dataset_epoch integer NOT NULL DEFAULT 1 CHECK (dataset_epoch >= 1),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- One row per (project, currency, kind): the chart of accounts (D-012).
CREATE TABLE accounts (
    project_id uuid NOT NULL REFERENCES projects (id),
    currency text NOT NULL CHECK (currency IN ('USD', 'EUR', 'JPY', 'KWD')),
    kind text NOT NULL CHECK (kind IN (
        'payer_funding', 'payer_held', 'merchant_pending', 'merchant_available',
        'fees', 'refunds_clearing', 'network_settlement')),
    PRIMARY KEY (project_id, currency, kind)
);

CREATE TABLE journal_entries (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    -- Allocation order; with posted_at it orders history. Never reused.
    seq bigint GENERATED ALWAYS AS IDENTITY,
    dataset_epoch integer NOT NULL CHECK (dataset_epoch >= 1),
    currency text NOT NULL CHECK (currency IN ('USD', 'EUR', 'JPY', 'KWD')),
    recipe text NOT NULL CHECK (recipe IN ('authorize', 'capture', 'void', 'refund', 'reversal')),
    semantics_version integer NOT NULL CHECK (semantics_version >= 1),
    operation_id uuid,
    payment_id uuid,
    reverses_entry_id uuid,
    posted_at timestamptz NOT NULL DEFAULT now(),
    created_xact xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY (project_id, id),
    UNIQUE (seq),
    UNIQUE (project_id, id, currency, dataset_epoch),
    FOREIGN KEY (project_id, reverses_entry_id) REFERENCES journal_entries (project_id, id),
    CHECK ((recipe = 'reversal') = (reverses_entry_id IS NOT NULL))
);

CREATE INDEX journal_entries_by_payment ON journal_entries (project_id, payment_id, seq)
    WHERE payment_id IS NOT NULL;

CREATE TABLE journal_lines (
    project_id uuid NOT NULL,
    entry_id uuid NOT NULL,
    line_no smallint NOT NULL CHECK (line_no BETWEEN 1 AND 8),
    currency text NOT NULL,
    dataset_epoch integer NOT NULL,
    account_kind text NOT NULL,
    -- Signed minor units: debit positive, credit negative.
    amount_minor bigint NOT NULL CHECK (
        amount_minor <> 0 AND amount_minor BETWEEN -1000000000000 AND 1000000000000),
    PRIMARY KEY (project_id, entry_id, line_no),
    FOREIGN KEY (project_id, entry_id, currency, dataset_epoch)
        REFERENCES journal_entries (project_id, id, currency, dataset_epoch),
    FOREIGN KEY (project_id, currency, account_kind) REFERENCES accounts (project_id, currency, kind)
);

-- Balances are sums over this index (D-028); INCLUDE makes them index-only scans.
CREATE INDEX journal_lines_by_account
    ON journal_lines (project_id, dataset_epoch, currency, account_kind) INCLUDE (amount_minor);

CREATE FUNCTION journal_refuse_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the journal is append-only: % on % refused', TG_OP, TG_TABLE_NAME
        USING ERRCODE = 'PSJ01',
              HINT = 'correct a posted entry with a reversing entry';
END
$$;

CREATE TRIGGER journal_entries_append_only
    BEFORE UPDATE OR DELETE ON journal_entries
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER journal_entries_no_truncate
    BEFORE TRUNCATE ON journal_entries
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER journal_lines_append_only
    BEFORE UPDATE OR DELETE ON journal_lines
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER journal_lines_no_truncate
    BEFORE TRUNCATE ON journal_lines
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();

-- The database, not the caller, stamps when and in which transaction an entry was posted.
CREATE FUNCTION journal_entries_stamp() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.posted_at := now();
    NEW.created_xact := pg_current_xact_id();
    RETURN NEW;
END
$$;

CREATE TRIGGER journal_entries_stamp
    BEFORE INSERT ON journal_entries
    FOR EACH ROW EXECUTE FUNCTION journal_entries_stamp();

-- After every INSERT statement into journal_lines: each touched entry balances, has at least
-- two lines, and was created in this transaction.
CREATE FUNCTION journal_lines_check_balance() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    offending uuid;
BEGIN
    SELECT e.id INTO offending
      FROM (SELECT DISTINCT project_id, entry_id FROM new_lines) touched
      JOIN journal_entries e ON e.project_id = touched.project_id AND e.id = touched.entry_id
      JOIN journal_lines l ON l.project_id = touched.project_id AND l.entry_id = touched.entry_id
     GROUP BY e.project_id, e.id, e.created_xact
    HAVING sum(l.amount_minor) <> 0
        OR count(*) < 2
        OR e.created_xact <> pg_current_xact_id()
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'journal entry % is unbalanced, has fewer than two lines, or was posted earlier', offending
            USING ERRCODE = 'PSJ02';
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER journal_lines_balanced
    AFTER INSERT ON journal_lines
    REFERENCING NEW TABLE AS new_lines
    FOR EACH STATEMENT EXECUTE FUNCTION journal_lines_check_balance();

-- At commit: every entry inserted in the transaction has its (balanced) lines.
CREATE FUNCTION journal_entries_check_lines() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    line_count bigint;
    total numeric;
BEGIN
    SELECT count(*), coalesce(sum(amount_minor), 0) INTO line_count, total
      FROM journal_lines
     WHERE project_id = NEW.project_id AND entry_id = NEW.id;
    IF line_count < 2 OR total <> 0 THEN
        RAISE EXCEPTION 'journal entry % has no balanced lines at commit', NEW.id
            USING ERRCODE = 'PSJ03';
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER journal_entries_have_lines
    AFTER INSERT ON journal_entries
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION journal_entries_check_lines();
