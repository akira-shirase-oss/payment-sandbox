-- Payment Sandbox, migration 5: the server's grants (D-038). SIMULATED MONEY ONLY.
-- The server connects as ps_app, a member of ps_runtime; it owns nothing, so it cannot
-- disable a trigger, set session_replication_role, TRUNCATE, or change the schema.
-- Roles come from `sandbox-server setup` (sandbox_db::roles::bootstrap).

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO ps_runtime;

-- Append-only history: read and insert, nothing else.
GRANT SELECT, INSERT ON journal_entries, journal_lines, events, audit_events, accounts, projects
    TO ps_runtime;
-- Mutable state: read, insert, update (never delete; the triggers refuse it anyway).
GRANT SELECT, INSERT, UPDATE ON payments, operations, api_keys, webhook_endpoints, deliveries
    TO ps_runtime;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO ps_runtime;
