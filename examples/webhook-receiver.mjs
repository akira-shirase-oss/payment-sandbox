// A webhook receiver from docs/DEVELOPERS.md, run as written by CI against a real sandbox.
// Needs PS_API_URL, PS_API_KEY (an admin key: it registers its own endpoint) and
// RECEIVER_PORT, a loopback port the server allows (PS_WEBHOOK_PORTS; 0 picks a free one,
// for a server in the dev profile, which allows any).
// SIMULATED MONEY ONLY: no real cards, balances or banking connection.
import { createServer } from 'node:http';
import { Sandbox, WebhookVerificationError, verifyWebhook } from '@sandbox/sdk';

const sandbox = new Sandbox({ apiUrl: process.env.PS_API_URL, apiKey: process.env.PS_API_KEY });
const seen = new Set(); // your store keeps the ids it handled in its database (an inbox)
let secret; // the endpoint's signing secret, from its registration below

let fulfil;
const captured = new Promise((resolve) => (fulfil = resolve));
const server = createServer((request, response) => {
	let body = '';
	request.setEncoding('utf8');
	request.on('data', (chunk) => (body += chunk));
	request.on('end', async () => {
		let event;
		try {
			// Verify the raw body before parsing anything.
			event = verifyWebhook({ body, headers: request.headers, secrets: [secret] });
		} catch (error) {
			if (!(error instanceof WebhookVerificationError)) throw error;
			response.writeHead(400).end();
			return;
		}
		response.writeHead(200).end(); // acknowledge at once; do the work after
		if (seen.has(event.id)) return; // deliveries are at least once: a repeat is normal
		seen.add(event.id);
		// Deliveries can arrive out of order: re-read the payment for its current state.
		const { payment } = await sandbox.payments.get(event.payment_id);
		console.log(`${event.type}: payment ${payment.status}`);
		if (payment.status === 'captured') fulfil(payment);
	});
});
await new Promise((resolve, reject) => {
	server.once('error', reject);
	server.listen(Number(process.env.RECEIVER_PORT ?? 56100), '127.0.0.1', resolve);
});

// Register the endpoint once and keep its secret; it is shown only in this answer.
const { port } = server.address();
({ secret } = await sandbox.webhookEndpoints.create(`http://127.0.0.1:${port}/webhooks`));

const hold = await sandbox.payments.create({ amount: 1500, currency: 'USD', description: 'Order 1005' });
await sandbox.payments.capture(hold.payment.id, 1500);
const timeout = new Promise((_, reject) => setTimeout(() => reject(new Error('no capture event within 30 s')), 30_000).unref());
const payment = await Promise.race([captured, timeout]);
console.log(`fulfil order 1005: payment ${payment.id} captured`);
server.close();
server.closeAllConnections();
