# @sandbox/sdk

> **SIMULATED MONEY ONLY.** Payment Sandbox moves no real money and accepts no real cards.

The TypeScript SDK a test store's server uses to take simulated payments from Payment
Sandbox and to verify its signed webhooks. Node 22.17 or later.

```js
import { Sandbox, verifyWebhook } from '@sandbox/sdk';

const sandbox = new Sandbox({ apiUrl: 'http://127.0.0.1:56080', apiKey: process.env.PS_API_KEY });
const hold = await sandbox.payments.create({ amount: 4200, currency: 'USD' });
if (!hold.settled) await sandbox.payments.waitForSettlement(hold.payment.id);
```

- **Refuses production-looking configuration** (D-068): a key that is not `sk_sim_…`, a real
  processor's host, `NODE_ENV=production`, or a host other than this machine (unless
  `allowNonLocal: true`, for a CI service container or a shared sandbox you run), and plain
  `http://` to such a host unless `allowPlainHttp: true` says the network is private (D-079).
- **Retries safely**: every money-moving call carries an idempotency key and every retry
  reuses it; at most 4 attempts with jittered backoff inside a 30 s deadline.
- **Honest outcomes**: `outcome: 'uncertain'` with `settled: false` is not a success;
  `UnknownOutcomeError` names the key to retry with.
- **Webhooks**: `verifyWebhook({ body, headers, secrets })` checks the HMAC-SHA256 signature
  and the 300 s window before you parse anything.

The full guide, with examples CI runs as written, is `docs/DEVELOPERS.md` in the repository.
