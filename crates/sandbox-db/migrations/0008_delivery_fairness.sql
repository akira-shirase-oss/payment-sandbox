-- Payment Sandbox, migration 8: fair delivery and a circuit breaker per endpoint (review
-- finding 12; D-049). SIMULATED MONEY ONLY.
--
-- `consecutive_failures` counts failed attempts since the last success; once it reaches
-- the breaker's threshold the circuit opens until `circuit_open_until`, a cooldown that
-- doubles with each opening (`circuit_opens`). While open, the endpoint's deliveries are
-- not claimed (and their attempts are not spent); after it, one probe at a time; a success
-- resets all three. The index serves claims that take the endpoints in turn.

ALTER TABLE webhook_endpoints
    ADD COLUMN consecutive_failures integer NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    ADD COLUMN circuit_opens integer NOT NULL DEFAULT 0 CHECK (circuit_opens >= 0),
    ADD COLUMN circuit_open_until timestamptz;

CREATE INDEX deliveries_due_by_endpoint
    ON deliveries (project_id, endpoint_id, next_attempt_at, event_id) WHERE state = 'pending';
