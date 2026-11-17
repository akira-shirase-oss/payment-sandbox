// The SDK against the real server, network and store that scripts/e2e.sh starts (M7,
// D-068): a purchase, a decline, an uncertain answer settled by the reconciler, a lost
// response retried without a second charge, a conflict, signed webhooks verified as a test
// store's server would, and a typed reset. SIMULATED MONEY ONLY.
import { createServer, type IncomingMessage } from 'node:http';
import { createServer as createTcpServer, connect, type Socket } from 'node:net';
import { describe, expect, it } from 'vitest';
import { ConfigError, RefusedError, SANDBOX_NOTICE, Sandbox, verifyWebhook, type SandboxEvent } from '../../src/index.ts';

function required(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is not set: run the live tests with scripts/e2e.sh`);
	return value;
}
const api = required('E2E_API');
const bootstrap = required('E2E_BOOTSTRAP');
const webhookPort = Number(required('E2E_SDK_WEBHOOK_PORT'));

let counter = 0;
async function project(): Promise<{ slug: string; admin: string }> {
	counter += 1;
	const slug = `sdk-${Date.now().toString(36)}-${counter}`;
	const response = await fetch(`${api}/v1/projects`, {
		method: 'POST',
		headers: { authorization: `Bearer ${bootstrap}`, 'content-type': 'application/json' },
		body: JSON.stringify({ slug, name: slug })
	});
	expect(response.status).toBe(201);
	return { slug, admin: ((await response.json()) as { api_key: { secret: string } }).api_key.secret };
}

/** A proxy in front of the API that loses the answer to the first payment creation. */
async function lossyProxy(): Promise<{ url: string; close: () => void; lost: () => number }> {
	const target = new URL(api);
	let creations = 0;
	const server = createTcpServer((client: Socket) => {
		const upstream = connect(Number(target.port), target.hostname);
		let request = '';
		client.on('data', (chunk) => {
			request += chunk.toString('latin1');
			upstream.write(chunk);
		});
		upstream.on('data', (chunk) => {
			if (request.startsWith('POST /v1/payments ') && creations++ === 0) {
				client.destroy();
				upstream.destroy();
				return;
			}
			client.write(chunk);
		});
		upstream.on('error', () => client.destroy());
		client.on('error', () => upstream.destroy());
		upstream.on('close', () => client.destroy());
	});
	await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
	const port = (server.address() as { port: number }).port;
	return { url: `http://127.0.0.1:${port}`, close: () => server.close(), lost: () => creations };
}

describe('the SDK against the real sandbox', () => {
	it('purchases, declines and settles an uncertain answer honestly', async () => {
		const { admin } = await project();
		const sandbox = new Sandbox({ apiUrl: api, apiKey: admin });
		const notice = await sandbox.notice();
		expect([notice.simulated, notice.notice]).toEqual([true, SANDBOX_NOTICE]);
		const hold = await sandbox.payments.create({ amount: 4_200, currency: 'USD', description: 'Order 1' });
		expect([hold.outcome, hold.settled, hold.status]).toEqual(['succeeded', true, 201]);
		expect((await sandbox.payments.capture(hold.payment.id, 4_200)).payment.status).toBe('captured');
		expect((await sandbox.payments.refund(hold.payment.id, 1_200)).payment.refunded).toBe(1_200);
		const tooMuch = await sandbox.payments.refund(hold.payment.id, 3_001).catch((e) => e);
		expect(tooMuch).toBeInstanceOf(RefusedError);
		expect(tooMuch.code).toBe('insufficient_capacity');

		const declined = await sandbox.payments.create({ amount: 999, currency: 'EUR', scenario: 'decline_do_not_honor' });
		expect([declined.outcome, declined.settled, declined.operation.decline_code]).toEqual(['declined', true, 'do_not_honor']);

		const lost = await sandbox.payments.create({ amount: 700, currency: 'USD', scenario: 'lost_reply' });
		expect([lost.outcome, lost.settled, lost.status]).toEqual(['uncertain', false, 202]);
		const settled = await sandbox.payments.waitForSettlement(lost.payment.id, { timeoutMs: 30_000 });
		expect(settled.settled).toBe(true);
		expect(settled.operations[0]?.state).toBe('succeeded');

		const again = await sandbox.payments.create({ amount: 4_200, currency: 'USD', description: 'Order 1' }, { idempotencyKey: hold.idempotencyKey });
		expect([again.replayed, again.status, again.payment.id]).toEqual([true, 200, hold.payment.id]);
		const conflict = await sandbox.payments.create({ amount: 4_300, currency: 'USD' }, { idempotencyKey: hold.idempotencyKey }).catch((e) => e);
		expect([conflict.status, conflict.code]).toEqual([409, 'idempotency_key_conflict']);
		expect((await sandbox.reconciliation({ network: true })).clean).toBe(true);
	});

	it('retries a lost response under the same key and charges once', async () => {
		const { admin } = await project();
		const proxy = await lossyProxy();
		try {
			const sandbox = new Sandbox({ apiUrl: proxy.url, apiKey: admin, retry: { baseMs: 10, capMs: 50 } });
			const result = await sandbox.payments.create({ amount: 1_234, currency: 'USD' });
			expect(proxy.lost()).toBe(2);
			expect([result.attempts, result.status, result.replayed]).toEqual([2, 200, true]);
			const direct = new Sandbox({ apiUrl: api, apiKey: admin });
			expect((await direct.payments.list()).payments.length).toBe(1);
		} finally {
			proxy.close();
		}
	});

	it('refuses production-looking configuration before sending anything', () => {
		expect(() => new Sandbox({ apiUrl: 'https://api.stripe.com', apiKey: `sk_sim_${'ab'.repeat(32)}`, allowNonLocal: true })).toThrow(ConfigError);
		const live = ['sk', 'live', 'ab'.repeat(32)].join('_');
		expect(() => new Sandbox({ apiUrl: api, apiKey: live })).toThrow(/not a Payment Sandbox key/);
	});

	it('verifies signed webhooks as a test store does', async () => {
		const { admin } = await project();
		const sandbox = new Sandbox({ apiUrl: api, apiKey: admin });
		const received: { body: string; headers: IncomingMessage['headers'] }[] = [];
		const server = createServer((request, response) => {
			let body = '';
			request.setEncoding('utf8');
			request.on('data', (chunk: string) => (body += chunk));
			request.on('end', () => {
				received.push({ body, headers: request.headers });
				response.writeHead(200, { 'content-type': 'application/json' }).end('{}');
			});
		});
		await new Promise<void>((resolve) => server.listen(webhookPort, '127.0.0.1', resolve));
		try {
			const { endpoint, secret } = await sandbox.webhookEndpoints.create(`http://127.0.0.1:${webhookPort}/hooks`);
			expect(secret).toMatch(/^whsec_sim_/);
			expect((await sandbox.webhookEndpoints.list()).endpoints.map((e) => e.id)).toEqual([endpoint.id]);
			const hold = await sandbox.payments.create({ amount: 500, currency: 'JPY' });
			await sandbox.payments.capture(hold.payment.id, 500);
			for (let i = 0; i < 100 && received.length < 2; i += 1) await new Promise((r) => setTimeout(r, 100));
			const events: SandboxEvent[] = received.map((r) => verifyWebhook({ body: r.body, headers: r.headers, secrets: [secret] }));
			expect(events.map((e) => `${e.sequence}:${e.type}`).sort()).toEqual(['1:authorize.succeeded', '2:capture.succeeded']);
			expect(events.every((e) => e.payment_id === hold.payment.id)).toBe(true);
			const first = received[0]!;
			expect(() => verifyWebhook({ body: `${first.body} `, headers: first.headers, secrets: [secret] })).toThrow(/No signature matches/);
			// A rotation keeps the old secret signing for a day; disabling stops deliveries.
			const rotated = await sandbox.webhookEndpoints.rotate(endpoint.id);
			expect([rotated.secret === secret, rotated.previousValidForSeconds]).toEqual([false, 86_400]);
			await sandbox.webhookEndpoints.disable(endpoint.id);
			expect((await sandbox.webhookEndpoints.list()).endpoints[0]!.disabled_at).not.toBeNull();
		} finally {
			server.close();
		}
	});

	it('creates and reads a hosted checkout session', async () => {
		const { admin } = await project();
		const sandbox = new Sandbox({ apiUrl: api, apiKey: admin });
		const created = await sandbox.checkoutSessions.create({ amount: 2500, currency: 'USD', description: 'Order 7', expiresIn: 600 });
		expect(created.url).toMatch(new RegExp(`/c/${created.session.id}#`));
		expect(created.url.endsWith(created.client_secret)).toBe(true);
		expect([created.session.state, created.session.capture_method, created.session.amount]).toEqual(['open', 'automatic', 2500]);
		const read = await sandbox.checkoutSessions.get(created.session.id);
		expect(read).not.toHaveProperty('client_secret');
		// Nobody pays it here: still open when the wait gives up, never "failed".
		const waited = await sandbox.checkoutSessions.waitForPayment(created.session.id, { timeoutMs: 300 });
		expect([waited.finished, waited.paid, waited.session.state]).toEqual([false, false, 'open']);
	});

	it('resets the dataset only with the typed slug and the current epoch', async () => {
		const { slug, admin } = await project();
		const sandbox = new Sandbox({ apiUrl: api, apiKey: admin });
		const old = await sandbox.payments.create({ amount: 100, currency: 'USD' });
		const wrong = await sandbox.resetDataset({ confirm: `${slug}x`, epoch: 1 }).catch((e) => e);
		expect([wrong.status, wrong.code]).toEqual([422, 'confirmation_mismatch']);
		const reset = await sandbox.resetDataset({ confirm: slug, epoch: (await sandbox.project()).dataset_epoch });
		expect([reset.from_epoch, reset.to_epoch, reset.archived_payments]).toEqual([1, 2, 1]);
		const twice = await sandbox.resetDataset({ confirm: slug, epoch: 1 }).catch((e) => e);
		expect([twice.status, twice.code]).toEqual([409, 'epoch_changed']);
		const archived = await sandbox.payments.get(old.payment.id).catch((e) => e);
		expect([archived.status, archived.code]).toEqual([410, 'dataset_archived']);
		expect((await sandbox.payments.list()).payments).toEqual([]);
	});
});
