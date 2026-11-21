# API reference

> **SIMULATED MONEY ONLY.** Every answer carries `Sandbox-Simulated: true` and every body
> `"simulated": true`; the clients stop at any answer without them.

The sandbox's HTTP API, version 1, on `http://127.0.0.1:56080` locally or behind your proxy's
`https://` name (`docs/OPERATING.md`). The TypeScript SDK (`packages/sdk-ts`) and
`sandbox-cli` cover every call a store makes; this page is for everything else, and for
reading what they send. A test keeps the table below and the server's routes identical
(`crates/sandbox-server/tests/api_reference.rs`): a route missing here, or a row the server
does not serve, fails the gate.

## Conventions

- **Authentication:** `Authorization: Bearer sk_sim_...`, a project's sandbox key. Each key
  has scopes: `read`, `payments:write` (grants `read` too) or `admin` (grants everything). Project
  creation alone takes the operator's bootstrap token (`PS_BOOTSTRAP_TOKEN`) instead.
- **Bodies:** JSON (`Content-Type: application/json`), at most 16 KiB by default
  (`PS_MAX_BODY_BYTES`); unknown fields are
  refused (`invalid_body`), and so is anything that looks like card data
  (`raw_card_data_rejected`, D-019: there is no card field anywhere).
- **Amounts** are integers in the currency's minor units: `4200` is 42.00 USD, `500` is
  500 JPY, `1250` is 1.250 KWD. Currencies: `USD`, `EUR`, `JPY`, `KWD`.
- **Idempotency:** every money-moving `POST` needs an `Idempotency-Key` header (1–255
  visible ASCII characters, nothing that looks like a card number). A repeat with the same key and body answers from the record
  (`200`, `"replayed": true`) or finishes the same operation; never a second charge. The same
  key with a different body is `409 idempotency_key_conflict`.
- **Outcomes:** `201` new, `200` replayed, **`202` not settled yet** (`operation.state` is
  `pending` or `uncertain`: the network's answer is not known; poll the payment). A `5xx`
  on a money-moving call may follow a commit: retry with the same key (D-075).
- **Errors** are one envelope:
  `{"error": {"code": "...", "message": "...", "request_id": "..."}, "simulated": true}`.
  `code` is stable; `message` is for people. `429` and `503` carry `Retry-After`.
- **Dataset epochs:** a reset (D-066) opens a new, empty epoch; anything from an earlier one
  answers `410 dataset_archived`.

## Endpoints

| Route | Scope | What it does |
|---|---|---|
| `GET /healthz` | none | Liveness: the process answers. |
| `GET /readyz` | none | Readiness: the database answers and every background task runs (`503` with a `reason` otherwise). |
| `GET /v1/sandbox` | none | The server's statement of what it is: `simulated`, `notice`, `ledger_semantics_version`. |
| `POST /v1/projects` | bootstrap token | Creates a project and its first admin key (shown once). |
| `GET /v1/project` | read | The key's project and its current `dataset_epoch`. |
| `POST /v1/project/reset` | admin | Opens a new, empty dataset epoch; nothing is deleted. |
| `GET /v1/keys` | admin | The project's keys (never their secrets). |
| `POST /v1/keys` | admin | Creates a key (shown once). |
| `POST /v1/keys/{key_id}/revoke` | admin | Revokes a key; it stops working at once. |
| `POST /v1/payments` | payments:write | Authorizes a payment: a hold, with a scenario the simulated network acts out. |
| `GET /v1/payments` | read | The most recent payments, newest first, a page at a time. |
| `GET /v1/payments/{payment_id}` | read | A payment and its operations, from one snapshot. |
| `GET /v1/payments/{payment_id}/journal` | read | The payment's journal entries, with the payment and its operations from the same snapshot. |
| `POST /v1/payments/{payment_id}/captures` | payments:write | Captures part or all of the hold. |
| `POST /v1/payments/{payment_id}/void` | payments:write | Releases the uncaptured remainder. |
| `POST /v1/payments/{payment_id}/refunds` | payments:write | Refunds captured money. |
| `GET /v1/balances` | read | Every account balance, derived from the journal and reconciled to it. |
| `GET /v1/balances/{currency}/{account}/explain` | read | The journal lines that make up one balance, a page at a time. |
| `GET /v1/reconciliation` | read | The reconciliation report (and, with `?network=true`, against the network's records). |
| `POST /v1/checkout_sessions` | payments:write | Creates a hosted checkout session; the answer holds the payer's URL. |
| `GET /v1/checkout_sessions/{id}` | read | A checkout session, its state and every attempt. |
| `GET /v1/webhook_endpoints` | admin | The project's webhook endpoints (never their secrets), a page at a time. |
| `POST /v1/webhook_endpoints` | admin | Registers an endpoint; its signing secret is in this answer only. |
| `POST /v1/webhook_endpoints/{endpoint_id}/rotate` | admin | A new signing secret; the old one keeps signing for 24 hours. |
| `POST /v1/webhook_endpoints/{endpoint_id}/disable` | admin | Stops deliveries to an endpoint. |
| `GET /v1/events` | read | Events (one per resolved operation) with their delivery states. |

The checkout (`PS_CHECKOUT_LISTEN`) and the dashboard (`PS_DASHBOARD_LISTEN`) are separate
origins with their own page-only APIs (D-061, D-062); scripts use `/v1` with a key.

## Projects and keys

`POST /v1/projects` — `Authorization: Bearer <PS_BOOTSTRAP_TOKEN>`; body
`{"slug": "my-store", "name": "My store"}` (slug: 1–63 lowercase letters, digits and `-`,
starting and ending with a letter or digit). `201` with `project` and `api_key` (`secret` shown once, `scopes: ["admin"]`). Without
the token: `401 bootstrap_token_required`; a taken slug: `409 conflict`.

`POST /v1/project/reset` — body `{"confirm": "<the project's slug>", "epoch": <current>}`.
`200` with `project` and `reset: {from_epoch, to_epoch, archived_payments}`. A wrong slug is
`422 confirmation_mismatch`; an epoch that is no longer current (a repeat) is
`409 epoch_changed`, never a second reset.

`POST /v1/keys` — body `{"label": "store", "scopes": ["payments:write"]}`. `201` with the
key and its `secret`, shown once. Revoked keys answer `401 unauthorized`.

## Payments

`POST /v1/payments` with `Idempotency-Key` — body
`{"amount": 4200, "currency": "USD", "token": "tok_sim_approve", "description": "Order 1"}`
(`description` optional, at most 200 characters). The token is the network's scenario:
`tok_sim_approve`, `tok_sim_decline_insufficient_funds`, `tok_sim_decline_do_not_honor`,
`tok_sim_decline_expired`, `tok_sim_lost_reply` (approved, the reply lost: `202 uncertain`
until the reconciler reads it back) and `tok_sim_unavailable` (`503` from the network, never
executed: `202 uncertain` until a fenced readback). The answer:
`{"payment": {...}, "operation": {...}, "replayed": false, "simulated": true}`.

- `payment`: `id`, `status` (`authorizing`, `declined`, `failed`, `authorized`,
  `partially_captured`, `captured`, `voided`, `partially_refunded`, `refunded`), `amount`,
  `currency`, `token`, `description`, `authorized`, `captured`,
  `voided`, `refunded`, `capturable`, `refundable`, `reserved` (by operations in flight),
  `decline_code`, `created_at`.
- `operation`: `id`, `kind` (`authorize`, `capture`, `void`, `refund`), `state` (`pending`,
  `uncertain`, `succeeded`, `declined`, `not_executed`), `amount`, `idempotency_key`,
  `decline_code`, `attempts`.

`POST .../captures` and `POST .../refunds` — `{"amount": 1200}`; `POST .../void` — `{}`; each
with its own `Idempotency-Key`. More than the authorized remainder (capture) or the captured
remainder (refund) is `409 insufficient_capacity`; simultaneous requests can never overrun.
A payment busy with other requests is `503 payment_busy`: retry with the same key.

`GET /v1/payments?limit=50&before=<payment id>` — `limit` 1–500; `next_before` names the
next page when this one is full.

## The ledger

`GET /v1/balances` — `{"balances": [{"currency", "account", "normal_side", "balance_minor",
"balance", "signed_minor"}]}`.

`GET /v1/balances/{currency}/{account}/explain?limit=100&cursor=...` — the journal lines that
sum to the balance with running totals (`explanation`); `next.cursor`, when present,
continues. A cursor from before a
restore is `409 stale_cursor`, one this server did not issue `400 invalid_cursor`: start
again. Reports have their own budget per project (`429 rate_limited`, `Retry-After`).

`GET /v1/reconciliation[?network=true]` — `{"clean": true, "report": {...}}`; with the
network, also `network` (operations one side has and the other lacks). `clean` is true only
when nothing disagrees.

## Hosted checkout

`POST /v1/checkout_sessions` — body:

| Field | Required | Meaning |
|---|---|---|
| `amount`, `currency` | yes | What the payer pays, in minor units. |
| `description` | no | Shown to the payer; at most 200 characters. |
| `capture_method` | no | `automatic` (default: captured as soon as authorized) or `manual` (you capture with `POST /v1/payments/{id}/captures`). |
| `return_url` | no | Where the payer's "Back to the store" link goes: an `http(s)` URL on a loopback host (D-023). |
| `embed_origins` | no | At most 4 loopback origins with a port (`http://localhost:3000`) allowed to frame the page; none by default. |
| `expires_in` | no | Seconds the session stays open: 60–86400, default 3600. |

`201` with the session view plus `client_secret` and `url`, the payer's link
(`<PS_CHECKOUT_ORIGIN>/c/<id>#<client secret>`); the secret is in the fragment, so it never
reaches a server log, and it is shown once. Send the payer to `url`. The view:
`session` (`id`, `amount`, `currency`, `capture_method`, `expires_at`, `expired`,
`attempts_left`, `state`, `next_action`), `attempt` (the latest: its number, scenario, the
authorize and capture states and any decline code), `captured`, `refunded`, and `attempts`
(every attempt, oldest first, with its `payment_id`). `state` is one of `open`, `processing`, `uncertain`, `declined`,
`not_executed`, `authorized`, `paid`, `conflict`: only `paid` (or `authorized` with manual
capture) is money you may act on.

`GET /v1/checkout_sessions/{id}` — the same view, without the secret. Poll it, or listen for
the payment's webhooks.

## Webhooks

`POST /v1/webhook_endpoints` — `{"url": "http://127.0.0.1:56100/webhooks"}`: loopback only,
on a port `PS_WEBHOOK_PORTS` allows (D-023, D-059). `201` with `endpoint` and `secret`
(`whsec_sim_...`, shown once). Deliveries are signed (`Sandbox-Signature`), at least once,
possibly out of order: verify the raw body (the SDK's `verifyWebhook`), deduplicate by event
id, and re-read the payment. An endpoint that keeps failing opens its circuit for a while;
`GET /v1/webhook_endpoints` shows `consecutive_failures` and `circuit_open_until`.

`GET /v1/events?payment_id=<id>&limit=100` — `{"events": [{"id", "kind", "payment_id",
"sequence", "deliveries": [[endpoint, state, attempts], ...]}]}`.

## Error codes

`bad_request`, `malformed_json`, `invalid_body`, `unsupported_media_type`,
`payload_too_large`, `body_timeout`, `idempotency_key_required`, `invalid_idempotency_key`,
`idempotency_key_conflict`, `raw_card_data_rejected`, `invalid_payment_token`,
`unauthorized`, `bootstrap_token_required`, `forbidden`, `route_not_found`,
`method_not_allowed`, `not_found`, `insufficient_capacity`, `invalid_state`, `stale_cursor`,
`invalid_cursor`, `cursor_expired`, `conflict`, `overloaded`, `rate_limited`, `payment_busy`,
`deadline_exceeded`, `unavailable`, `cross_origin_refused`, `checkout_attempt_open`,
`checkout_attempts_exhausted`, `checkout_expired`, `checkout_closed`,
`operator_credential_required`, `misdirected_request`, `confirmation_mismatch`,
`epoch_changed`, `dataset_archived`, `internal`.
