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
