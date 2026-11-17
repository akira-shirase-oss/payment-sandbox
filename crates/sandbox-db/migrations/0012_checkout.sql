-- Payment Sandbox, migration 12: checkout sessions and their attempts (D-060).
-- SIMULATED MONEY ONLY.
--
-- A merchant creates a session (amount, currency, description fixed by the merchant); the
-- payer's page authenticates with the session's client secret, whose SHA-256 alone is kept.
-- Every press of "Pay" is an attempt, committed under the session's row lock before any
-- payment work: its authorize operation and payment ids are derived from the session and
-- the attempt number (D-015), so they are known before the network is called. An attempt is
-- never changed or deleted (the runtime role may only insert and read them); a session's
-- only mutable column is its attempt count.

CREATE TABLE checkout_sessions (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    dataset_epoch integer NOT NULL CHECK (dataset_epoch >= 1),
    secret_hash bytea NOT NULL CHECK (octet_length(secret_hash) = 32),
    amount_minor bigint NOT NULL CHECK (amount_minor BETWEEN 1 AND 1000000000000),
    currency text NOT NULL CHECK (currency IN ('USD', 'EUR', 'JPY', 'KWD')),
    description text CHECK (char_length(description) <= 200),
    capture_method text NOT NULL CHECK (capture_method IN ('automatic', 'manual')),
    embed_origins text[] NOT NULL DEFAULT '{}' CHECK (cardinality(embed_origins) <= 4),
    return_url text CHECK (char_length(return_url) <= 2048),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 10),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (project_id, id),
    -- The payer's page knows only the session id (and its secret): one lookup by id.
    UNIQUE (id),
    CHECK (expires_at > created_at)
);

CREATE TABLE checkout_attempts (
    project_id uuid NOT NULL,
    session_id uuid NOT NULL,
    number integer NOT NULL CHECK (number BETWEEN 1 AND 10),
    client_key text NOT NULL CHECK (char_length(client_key) BETWEEN 1 AND 255),
    token text NOT NULL CHECK (token ~ '^tok_sim_[a-z0-9_]{1,56}$'),
    authorize_operation_id uuid NOT NULL,
    payment_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, session_id, number),
    UNIQUE (project_id, session_id, client_key),
    FOREIGN KEY (project_id, session_id) REFERENCES checkout_sessions (project_id, id)
);

GRANT SELECT, INSERT, UPDATE ON checkout_sessions TO ps_runtime;
GRANT SELECT, INSERT ON checkout_attempts TO ps_runtime;
