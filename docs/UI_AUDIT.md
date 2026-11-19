# UI audit — what the operator's day looks like, on screen

Every API route and CLI command an operator uses day to day, the dashboard
screen that does or shows it, and the Playwright coverage that proves the
screen exists and works. SIMULATED MONEY ONLY: every journey runs against the
real server, the simulated network and the development store
(`scripts/e2e.sh`), never a production route — none exists.

Coverage matrix required of every screen: the owner (admin key) and the viewer
(`scopes: ['read']` key) roles; 1440 px and 390 px widths; light and dark
schemes; and the empty, error and denied states. Specs are written and
syntax-checked in-repo (`npm run check --workspace @sandbox/e2e`); the
Chromium runs are the coordinator's.

## Operator routes → screens → specs

Dashboard session surface (what the SvelteKit app actually calls — `lib/api.ts`
prefixes `/api`, the server proxies them under the session cookie):

| Route (what it does) | Dashboard screen | Spec that covers it | Gap |
|---|---|---|---|
| `POST /api/session` — sign in with a sandbox key; `GET /api/session/current`; `POST /api/session/sign-out` | Sign-in card (`+layout.svelte`), then Overview | `dashboard.spec.ts` (cookie flags, sign-out ends server-side), `screens.spec.ts`, `audit.spec.ts` | — |
| `GET /api/status` — service health; `GET /api/v1/project` — project + epoch | Overview (`/`): reconciliation card, background-tasks card | `screens.spec.ts`, `audit.spec.ts` | — |
| `GET /api/v1/payments` (+ `/api/v1/payments/{id}` detail) | Payments list (`/payments`), payment detail (`/payments/[id]`) | `dashboard.spec.ts` (bounded list), `screens.spec.ts`, `audit.spec.ts` | — |
| `GET /api/v1/balances` (+ `…/explain`) | Ledger (`/ledger`), account explanation (`/ledger/[currency]/[account]`) | `dashboard.spec.ts` (paging until lines sum to the balance), `screens.spec.ts`, `audit.spec.ts` | — |
| `GET /api/v1/events` | Webhooks (`/webhooks`) | `screens.spec.ts`, `audit.spec.ts` | — |
| `GET|POST /api/v1/webhook_endpoints` (register/list) | Webhooks (`/webhooks`) | `screens.spec.ts`, `audit.spec.ts` | no spec drives the register-endpoint form |
| `GET /api/v1/reconciliation` | Overview reconciliation card; Dataset (epoch view) | `screens.spec.ts`, `audit.spec.ts` | — |
| `POST /api/v1/project/reset` (typed-slug confirmation, D-022) | Dataset (`/dataset`) | `dataset.spec.ts` (admin types slug, keyboard-only; raced reset; read key denied via `needs-admin`), `audit.spec.ts` | — |
| `GET|POST /api/sim/clock`, `GET|POST /api/sim/faults`, `POST /api/sim/faults/clear` (dev profile only) | Simulator (`/simulator`, nav entry gated by `sim_controls`) | `dashboard.spec.ts` (controls answer only the operator credential), `audit.spec.ts` | — |
| `GET /api/sessions/{id}`, `…/attempts`, `…/continue` (checkout sessions, merchant view) | Payments detail surfaces checkout attempts | `checkout.spec.ts` (merchant view in journeys) | no dashboard-side spec for the sessions section |

Public API routes (what the CLI/SDK/merchant backend calls — the dashboard's
session proxy is the only browser path to them):

| Route (what it does) | Where it shows | Spec | Gap |
|---|---|---|---|
| `POST /v1/projects` (operator bootstrap) | — | `support.ts` `project()` in every spec | — |
| `POST /v1/keys`, `POST /v1/keys/{id}/revoke` | — | `support.ts` `project()` creates the read key | revoke path: no spec |
| `GET /v1/project`, `POST /v1/project/reset` | Dataset | `dataset.spec.ts` | — |
| `GET /v1/balances`, `GET /v1/balances/{c}/{a}/explain` | Ledger | `dashboard.spec.ts`, `audit.spec.ts` | — |
| `POST /v1/payments`, `GET /v1/payments/{id}`, `POST …/captures`, `…/refunds`, `…/voids` | Payments (read side) | Journeys create them via API in `checkout.spec.ts`/`dataset.spec.ts`; `audit.spec.ts` lists them | void path: API-covered, no screen drive |
| `GET /v1/reconciliation[?network=true]` | Overview card | `audit.spec.ts` (heading); `checkout.spec.ts` (reconciler resolves uncertain) | — |
| `GET /v1/events`, `POST /v1/webhook_endpoints`, `POST …/rotate`, `POST …/disable` | Webhooks | `audit.spec.ts` (screen), webhook delivery itself: `checkout.spec.ts` fulfillment | rotate/disable forms: no spec |
| `POST /v1/checkout_sessions`, `GET /v1/checkout_sessions/{id}` | `/c/{id}` payer page (`apps/checkout`); attempts surface on Payments detail | `checkout.spec.ts` (full payer journey), `screens.spec.ts` | — |
| `GET /v1/sandbox` (sandbox label/about) | — | indirect | no spec |
| `GET /healthz`, `GET /readyz` | — | `scripts/e2e.sh` readiness waits | — |

## CLI commands → screens → specs

`ps` (`crates/sandbox-cli`) is the operator's other daily surface; its server
round-trips are API-covered in `crates/sandbox-cli/tests`. Screen column is
where the same work is visible in the dashboard.

| Command | Same work on screen | Coverage | Gap |
|---|---|---|---|
| `status` | Overview (project, epoch) | `audit.spec.ts` | — |
| `payments create|get|list|capture|void|refund|wait` | Payments list/detail | `audit.spec.ts`, `dashboard.spec.ts` | — |
| `ledger balances|journal|explain` | Ledger + account explanation | `dashboard.spec.ts` (sum to balance) | — |
| `reconcile [--network]` | Overview reconciliation card | `audit.spec.ts` | — |
| `checkout create|get|wait` | `/c/{id}` + Payments detail attempts | `checkout.spec.ts` | — |
| `webhooks create|list|rotate|disable` | Webhooks | `audit.spec.ts` (list side) | rotate/disable forms: no spec |
| `events [--payment] [--limit]` | Webhooks events | `audit.spec.ts` | — |
| `scenarios list|show|run` | — (operator/CLI only by design) | `crates/sandbox-cli` tests | — |
| `reset [--confirm slug]` | Dataset | `dataset.spec.ts` | — |

## The new systematic walk

`e2e/tests/audit.spec.ts` (this commit): for **owner** and **viewer** at
**1440 px and 390 px**, in **light and dark**: every section renders its
heading, its simulated-money framing and no error page; on a fresh project the
empty states render (payments, webhooks, ledger) — plus the viewer's denied
state (`needs-admin` on the reset form, covered in depth by
`dataset.spec.ts`).

## Deliberately out of scope

- Checkout (`/c/{id}`) payer screens — covered end-to-end by
  `checkout.spec.ts`, `screens.spec.ts`, `policy.spec.ts`, `a11y.spec.ts`,
  `phone.spec.ts`, `perf.spec.ts`, `vitals.spec.ts`.
- Performance/visual baselines — `perf.spec.ts`, `vitals.spec.ts`,
  `screens.spec.ts` own them.
