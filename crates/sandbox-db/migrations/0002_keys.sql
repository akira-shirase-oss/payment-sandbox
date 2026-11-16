-- Payment Sandbox, migration 2: sandbox API keys and the audit trail. SIMULATED MONEY ONLY.
-- Only the SHA-256 of a key is stored (D-021); the plaintext is shown once at creation.

CREATE TABLE api_keys (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    key_hash bytea NOT NULL UNIQUE CHECK (octet_length(key_hash) = 32),
    key_prefix text NOT NULL CHECK (key_prefix ~ '^sk_sim_[0-9a-f]{8}$'),
    scopes text[] NOT NULL CHECK (
        cardinality(scopes) BETWEEN 1 AND 3
        AND scopes <@ ARRAY['read', 'payments:write', 'admin']::text[]),
    label text NOT NULL CHECK (char_length(label) BETWEEN 1 AND 100),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    PRIMARY KEY (project_id, id)
);

-- Append-only record of administrative actions (key creation and revocation; dataset
-- resets later). Never updated or deleted.
CREATE TABLE audit_events (
    project_id uuid NOT NULL REFERENCES projects (id),
    id uuid NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    actor text NOT NULL CHECK (char_length(actor) BETWEEN 1 AND 100),
    action text NOT NULL CHECK (action IN ('project.created', 'key.created', 'key.revoked', 'dataset.reset')),
    subject_id uuid,
    PRIMARY KEY (project_id, id)
);

CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION journal_refuse_change();
CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION journal_refuse_change();
