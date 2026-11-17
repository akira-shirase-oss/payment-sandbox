# Developer guide

> **SIMULATED MONEY ONLY.** Payment Sandbox moves no real money and accepts no real cards.
> Nothing here may ever fulfill a real order.

Payment Sandbox lets a store's code take payments, see them fail in every way a real network
fails, and prove its books balance, without a payment account. This guide gets you from a
running sandbox to a store that handles declines, lost replies and duplicate webhooks. Every
example below is a file in `examples/` that CI runs, as written, against a real sandbox.

## 1. Run the sandbox

```sh
export PATH="$(brew --prefix rustup)/bin:$PATH"   # macOS with rustup: the pinned toolchain
scripts/stack.sh up && eval "$(scripts/stack.sh env)"
cargo run -p sandbox-server -- setup && cargo run -p sandbox-server -- migrate
export PS_BOOTSTRAP_TOKEN=$(openssl rand -hex 24) # the operator's token that creates projects
cargo run -p sandbox-sim &                        # the simulated network
PS_SECRET_KEY=$(openssl rand -hex 32) cargo run -p sandbox-server &   # the key signs webhooks
cargo install --path crates/sandbox-cli           # sandbox-cli on your PATH
```

Create a project with the operator's bootstrap token (it answers with an admin key, shown
once), then keep the key in your environment, never in a command line:

```sh
curl -s -X POST http://127.0.0.1:56080/v1/projects \
  -H "Authorization: Bearer $PS_BOOTSTRAP_TOKEN" -H 'Content-Type: application/json' \
  -d '{"slug": "my-store", "name": "My store"}'
export PS_API_KEY=sk_sim_...                      # the api_key.secret it printed
```

For a sandbox somewhere else (a deployed one, see `docs/OPERATING.md`) set `PS_API_URL` to
its `https://` name too, and pass `--allow-non-local`; if its proxy's certificate comes from
a private authority, give the CLI that authority's certificate with `--ca-cert` (or
`PS_CA_CERT`).

## 2. The CLI

`sandbox-cli --help` lists every command; each says what it does, and every error says what
went wrong and what to do. The quickstart:

<!-- example: examples/cli-quickstart.sh -->
```bash
#!/usr/bin/env bash
# The CLI quickstart from docs/DEVELOPERS.md, run as written by CI against a real sandbox.
# Needs a running sandbox and one of your project's admin keys:
#   export PS_API_URL=http://127.0.0.1:56080 PS_API_KEY=sk_sim_...
# SIMULATED MONEY ONLY: no real cards, balances or banking connection.
set -euo pipefail

sandbox-cli status

# A purchase: hold 42.00, capture it, refund 12.00.
payment=$(sandbox-cli --json payments create --amount 42.00 --currency USD --description "Order 1001" | jq -r .payment.id)
sandbox-cli payments capture "$payment" --amount 42.00
sandbox-cli payments refund "$payment" --amount 12.00

# Every balance is explained by journal lines, and the ledger reconciles.
sandbox-cli ledger explain USD merchant_pending
sandbox-cli reconcile

# A lost reply: exit status 4 means "not known yet", never "paid".
set +e
lost=$(sandbox-cli --json payments create --amount 7.00 --currency USD --scenario lost_reply)
status=$?
set -e
[ "$status" -eq 4 ]
sandbox-cli payments wait "$(jq -r .payment.id <<<"$lost")"

# Replay the failure library and check every trace against the committed one.
sandbox-cli scenarios run --all --check
```

Exit status: `0` done, `1` refused (the message says why), `2` wrong usage or configuration,
`3` declined or never executed, `4` **not known yet** (uncertain, or no usable answer to a
money-moving command after its retries: the message names the `--idempotency-key` to retry
with), `5` the sandbox could not be reached. Plain words by default; `--json` prints the
API's JSON.

## 3. The SDK in your store's server

`packages/sdk-ts` (`@sandbox/sdk`, Node 22.17+). Amounts are minor units (4200 = 42.00 USD).

<!-- example: examples/sdk-purchase.mjs -->
```js
// The SDK quickstart from docs/DEVELOPERS.md, run as written by CI against a real sandbox.
// Needs PS_API_URL and PS_API_KEY (one of your project's keys with payments:write).
// SIMULATED MONEY ONLY: no real cards, balances or banking connection.
import { RefusedError, Sandbox } from '@sandbox/sdk';

const sandbox = new Sandbox({ apiUrl: process.env.PS_API_URL, apiKey: process.env.PS_API_KEY });

// Authorize 42.00 USD (amounts are minor units). Keep the idempotency key with the order:
// retrying with it can never charge twice.
const hold = await sandbox.payments.create({ amount: 4200, currency: 'USD', description: 'Order 1002' });
console.log(`authorize: ${hold.outcome} (key ${hold.idempotencyKey})`);

const capture = await sandbox.payments.capture(hold.payment.id, 4200);
console.log(`capture: ${capture.outcome}, payment ${capture.payment.status}`);

// Refunds cannot exceed what was captured: the excess is refused, never overrun.
try {
	await sandbox.payments.refund(hold.payment.id, 5000);
} catch (error) {
	if (!(error instanceof RefusedError)) throw error;
	console.log(`refund of 50.00 refused: ${error.code}`);
}

// An uncertain answer is not a success: wait until the reconciler settles it.
const lost = await sandbox.payments.create({ amount: 700, currency: 'USD', scenario: 'lost_reply' });
console.log(`lost reply: ${lost.outcome}, settled ${lost.settled}`);
const settled = await sandbox.payments.waitForSettlement(lost.payment.id, { timeoutMs: 30_000 });
console.log(`after the reconciler: settled ${settled.settled}, ${settled.operations[0].state}`);

const report = await sandbox.reconciliation();
if (!report.clean) throw new Error('the ledger did not reconcile');
console.log('reconciliation: clean');
```

### What "done" means

| The call says | It means | Do |
|---|---|---|
| `outcome: 'succeeded'`, `settled: true` | the simulated network approved; the journal has the entry | carry on |
| `outcome: 'declined'` | the network said no (`operation.decline_code`) | ask the payer for another scenario |
| `outcome: 'not_executed'` | proved never executed (a fenced readback) | nothing moved; start a new payment |
| `outcome: 'uncertain'` or `'pending'`, `settled: false` | the network's answer is not known yet | `waitForSettlement`; never treat it as paid |
| `UnknownOutcomeError` (`settled: false`) | no usable answer after the retries: none at all, a server error, or a proxy answering `502`/`504` while the server restarts | retry with `error.idempotencyKey`, never a new key: the sandbox answers from its record or finishes the same operation |
| `RefusedError` (`code`, `status`) | the sandbox refused: capacity, conflict, scope, ... | fix the request; `code` is stable |

Retries are already done for you, and safely: every money-moving call carries an
idempotency key, every retry reuses it, and the sandbox answers a repeat from its record
(`replayed: true`) instead of charging again. The same key with a different amount is a
conflict, never a reinterpretation.

### Production-looking configuration is refused

The SDK and the CLI refuse, before sending anything: a key that is not `sk_sim_` followed by
64 hex characters (a real processor's key never travels), a real processor's host, a host
that is not this machine unless you pass `allowNonLocal: true` / `--allow-non-local` (a CI
service container, or a sandbox you deployed), and, in the SDK, `NODE_ENV=production`
unless `allowProductionEnvironment: true` says this is a test of a production build. Every
answer must carry `Sandbox-Simulated: true`, or the client stops.

## 4. Webhooks

Register an endpoint (`POST /v1/webhook_endpoints {"url": ...}` with an admin key; in the
local mode only loopback receivers on allowed ports, D-023 and D-059) and keep the
`whsec_sim_...` secret it returns. Each delivery is signed; verify the raw body before
parsing it, and deduplicate by event id: deliveries are at least once, and they can arrive
out of order.

```js
import { verifyWebhook } from '@sandbox/sdk';

const event = verifyWebhook({ body: rawBody, headers: request.headers, secrets: [secret] });
// event.type is e.g. 'capture.succeeded'; re-read the payment for its current state:
const { payment } = await sandbox.payments.get(event.payment_id);
```

`fixtures/dev-store` is a complete reference consumer: inbox deduplication, the re-read,
and fulfillment under its own unique key.

## 5. Failure scenarios, reproducibly

`sandbox-cli scenarios list` shows the seeded library (`fixtures/scenarios/`): declines,
an expired method, a lost reply, an unavailable network, capacity refusals, idempotent
repeats, capture over authorization, zero- and three-decimal currencies. `sandbox-cli
scenarios run <name> --check` runs one with fresh keys and compares what happened with the
trace committed beside it; the same scenario gives the same states, events and journal
entries on every run. Write your own as JSON in the same shape and run it with `--file`.

## 6. Start over

`sandbox-cli reset` (or the dashboard's Dataset page) opens a new, empty dataset epoch after
you type the project's slug; it needs an admin key. Nothing is deleted: the earlier epoch's
journal, payments and events stay archived, a payment of it answers `410 dataset_archived`,
and the same idempotency keys start fresh.
