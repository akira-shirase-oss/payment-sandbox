-- Payment Sandbox, migration 9: one clock for delivery schedules (review finding 20; D-052).
-- SIMULATED MONEY ONLY.
--
-- A new delivery's next_attempt_at defaulted to the database's now(), while claims compare
-- it with the deliverer's clock, so a deliverer clock behind the database's stalled new
-- deliveries until it caught up. A new delivery is now due at '-infinity': at once, on any
-- clock. Every later value (retries, leases, the breaker) is written by the deliverer's
-- clock and compared on it.

ALTER TABLE deliveries ALTER COLUMN next_attempt_at SET DEFAULT '-infinity';
