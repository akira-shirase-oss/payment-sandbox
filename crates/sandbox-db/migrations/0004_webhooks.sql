-- Payment Sandbox, migration 4: signed webhooks through an outbox (D-006, D-020, D-025,
-- D-032). SIMULATED MONEY ONLY.
--
-- Every resolved operation commits exactly one event, in the same transaction as the state
-- change, with a per-payment sequence; one delivery row per active endpoint is created with
-- it. The deliverer claims due deliveries under a lease; every claim bumps `lease_token`,
-- and a result is recorded only by the holder of the current token (fencing), so a stale
-- worker can never overwrite a newer attempt.

CREATE TABLE webhook_endpoints (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    url text NOT NULL CHECK (char_length(url) BETWEEN 10 AND 2048 AND url LIKE 'http://%'),
    -- ChaCha20-Poly1305 under the server key; the row's ids are the associated data.
    secret_ciphertext bytea NOT NULL,
    secret_nonce bytea NOT NULL CHECK (octet_length(secret_nonce) = 12),
    previous_ciphertext bytea,
    previous_nonce bytea CHECK (previous_nonce IS NULL OR octet_length(previous_nonce) = 12),
    previous_expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz,
    PRIMARY KEY (project_id, id),
    CHECK ((previous_ciphertext IS NULL) = (previous_nonce IS NULL)
       AND (previous_ciphertext IS NULL) = (previous_expires_at IS NULL))
);

ALTER TABLE payments ADD COLUMN event_sequence bigint NOT NULL DEFAULT 0 CHECK (event_sequence >= 0);

CREATE TABLE events (
    project_id uuid NOT NULL,
    id uuid NOT NULL,
    dataset_epoch integer NOT NULL CHECK (dataset_epoch >= 1),
    payment_id uuid NOT NULL,
    operation_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind ~ '^(authorize|capture|void|refund)\.(succeeded|declined|not_executed)$'),
    sequence bigint NOT NULL CHECK (sequence >= 1),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, operation_id),
    UNIQUE (project_id, payment_id, sequence),
    FOREIGN KEY (project_id, payment_id) REFERENCES payments (project_id, id),
    FOREIGN KEY (project_id, operation_id) REFERENCES operations (project_id, id)
);

CREATE TRIGGER events_append_only BEFORE UPDATE OR DELETE ON events
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER events_no_truncate BEFORE TRUNCATE ON events
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();

CREATE TABLE deliveries (
    project_id uuid NOT NULL,
    event_id uuid NOT NULL,
    endpoint_id uuid NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered', 'dead')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 12),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    lease_until timestamptz,
    lease_token bigint NOT NULL DEFAULT 0,
    last_status integer,
    last_error text CHECK (char_length(last_error) <= 300),
    delivered_at timestamptz,
    PRIMARY KEY (project_id, event_id, endpoint_id),
    FOREIGN KEY (project_id, event_id) REFERENCES events (project_id, id),
    FOREIGN KEY (project_id, endpoint_id) REFERENCES webhook_endpoints (project_id, id)
);

CREATE INDEX deliveries_due ON deliveries (next_attempt_at) WHERE state = 'pending';
