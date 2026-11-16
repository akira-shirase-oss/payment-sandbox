-- Payment Sandbox, migration 3: payments and operations (D-011, D-014, D-015, D-016).
-- SIMULATED MONEY ONLY.
--
-- A payment is the parent; each authorize, capture, void and refund is an operation with an
-- identity derived from the client's Idempotency-Key. The payment row holds the committed
-- sums (equal to its journal entries, which reconciliation proves) and the in-flight
-- reservations. CHECK constraints make overrunning capacity impossible, whatever the code
-- does: captured + voided + reservations <= authorized, refunded + reservation <= captured.

CREATE TABLE payments (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    dataset_epoch integer NOT NULL CHECK (dataset_epoch >= 1),
    authorize_operation_id uuid NOT NULL,
    currency text NOT NULL CHECK (currency IN ('USD', 'EUR', 'JPY', 'KWD')),
    amount_minor bigint NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000),
    token text NOT NULL CHECK (token ~ '^tok_sim_[a-z0-9_]{1,56}$'),
    description text CHECK (char_length(description) <= 200),
    status text NOT NULL CHECK (status IN (
        'authorizing', 'declined', 'failed', 'authorized', 'partially_captured', 'captured',
        'voided', 'partially_refunded', 'refunded')),
    authorized_minor bigint NOT NULL DEFAULT 0,
    captured_minor bigint NOT NULL DEFAULT 0,
    capture_reserved_minor bigint NOT NULL DEFAULT 0,
    voided_minor bigint NOT NULL DEFAULT 0,
    void_reserved_minor bigint NOT NULL DEFAULT 0,
    refunded_minor bigint NOT NULL DEFAULT 0,
    refund_reserved_minor bigint NOT NULL DEFAULT 0,
    decline_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, authorize_operation_id),
    CHECK (authorized_minor IN (0, amount_minor)),
    CHECK (captured_minor >= 0 AND capture_reserved_minor >= 0 AND voided_minor >= 0
           AND void_reserved_minor >= 0 AND refunded_minor >= 0 AND refund_reserved_minor >= 0),
    CONSTRAINT payments_capture_capacity CHECK (
        captured_minor + capture_reserved_minor + voided_minor + void_reserved_minor <= authorized_minor),
    CONSTRAINT payments_refund_capacity CHECK (
        refunded_minor + refund_reserved_minor <= captured_minor)
);

CREATE INDEX payments_recent ON payments (project_id, dataset_epoch, created_at DESC);

CREATE TABLE operations (
    project_id uuid NOT NULL,
    id uuid NOT NULL,
    dataset_epoch integer NOT NULL CHECK (dataset_epoch >= 1),
    payment_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('authorize', 'capture', 'void', 'refund')),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 255),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    amount_minor bigint NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000),
    state text NOT NULL CHECK (state IN ('pending', 'uncertain', 'succeeded', 'declined', 'not_executed')),
    decline_code text,
    network_ref text,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, dataset_epoch, kind, idempotency_key),
    FOREIGN KEY (project_id, payment_id) REFERENCES payments (project_id, id),
    CHECK ((state IN ('succeeded', 'declined', 'not_executed')) = (resolved_at IS NOT NULL))
);

CREATE INDEX operations_open ON operations (created_at) WHERE state IN ('pending', 'uncertain');
CREATE INDEX operations_by_payment ON operations (project_id, payment_id, created_at);

-- An operation's answer is applied exactly once: a resolved operation never changes again.
CREATE FUNCTION operations_resolved_is_final() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state IN ('succeeded', 'declined', 'not_executed') THEN
        RAISE EXCEPTION 'operation % is already resolved as %', OLD.id, OLD.state
            USING ERRCODE = 'PSO01';
    END IF;
    IF NEW.id <> OLD.id OR NEW.kind <> OLD.kind OR NEW.amount_minor <> OLD.amount_minor
       OR NEW.fingerprint <> OLD.fingerprint OR NEW.payment_id <> OLD.payment_id
       OR NEW.idempotency_key <> OLD.idempotency_key THEN
        RAISE EXCEPTION 'operation % identity fields are immutable', OLD.id
            USING ERRCODE = 'PSO01';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER operations_resolved_is_final
    BEFORE UPDATE ON operations
    FOR EACH ROW EXECUTE FUNCTION operations_resolved_is_final();

-- Payments and operations are never deleted (a reset opens a new epoch, D-022).
CREATE TRIGGER payments_no_delete BEFORE DELETE ON payments
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER payments_no_truncate BEFORE TRUNCATE ON payments
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER operations_no_delete BEFORE DELETE ON operations
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER operations_no_truncate BEFORE TRUNCATE ON operations
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();

-- The journal now references what it records; one entry per operation at most.
ALTER TABLE journal_entries
    ADD CONSTRAINT journal_entries_operation_fk
        FOREIGN KEY (project_id, operation_id) REFERENCES operations (project_id, id),
    ADD CONSTRAINT journal_entries_payment_fk
        FOREIGN KEY (project_id, payment_id) REFERENCES payments (project_id, id);
CREATE UNIQUE INDEX journal_entries_one_per_operation
    ON journal_entries (project_id, operation_id) WHERE operation_id IS NOT NULL;
