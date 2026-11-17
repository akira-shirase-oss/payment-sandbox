// The hosted checkout from docs/DEVELOPERS.md, run as written by CI against a real sandbox.
// Needs PS_API_URL and PS_API_KEY (one of your project's keys with payments:write). A payer
// opens the printed link and chooses what the simulated network does; in CI a stand-in payer
// (e2e/payer.mjs) does it from the terminal.
// SIMULATED MONEY ONLY: no real cards, balances or banking connection.
import { Sandbox } from '@sandbox/sdk';

const sandbox = new Sandbox({ apiUrl: process.env.PS_API_URL, apiKey: process.env.PS_API_KEY });

// Your store's server creates a session for the order and sends the payer to its url. The
// link holds the session's secret: give it to that payer only. No money moves yet.
const created = await sandbox.checkoutSessions.create({
	amount: 2500,
	currency: 'USD',
	description: 'Order 1003',
	expiresIn: 900
});
console.log(`send the payer to: ${created.url}`);

// Then it waits for the outcome (or listens for the payment's webhooks). Only "paid" is money
// to act on; a decline lets the payer try again, and "uncertain" is not known yet.
const result = await sandbox.checkoutSessions.waitForPayment(created.session.id, { timeoutMs: 60_000 });
console.log(`checkout ${result.session.state}, finished ${result.finished}`);
if (!result.paid) throw new Error('not paid: do not fulfil the order');

// The session names its payment: the same journal-backed record as any other.
const { payment } = await sandbox.payments.get(result.attempt.payment_id);
console.log(`payment ${payment.status}: captured ${payment.captured} of ${payment.amount}`);
