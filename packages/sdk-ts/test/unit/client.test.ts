// Retries are bounded and reuse one key; answers are honest (D-068). A fake fetch plays
// the sandbox, so every branch is exercised without a server; test/live does the same
// against the real one.
import { describe, expect, it } from 'vitest';
import {
	NotASandboxError,
	RefusedError,
	Sandbox,
	UnknownOutcomeError,
	UnreachableError,
	backoffMs,
	newIdempotencyKey,
	retryable,
	DEFAULT_RETRY
} from '../../src/index.ts';

const KEY = `sk_sim_${'0123456789abcdef'.repeat(4)}`;
const FAST = { baseMs: 1, capMs: 5 };

type Reply = { status: number; body?: unknown; simulated?: boolean } | 'refused' | 'reset';

/** A fetch that answers `replies` in turn (the last forever) and records each request. */
function fake(replies: Reply[]) {
	const seen: { url: string; key: string | null; method: string }[] = [];
	const fetch = (async (url: string | URL, init?: RequestInit) => {
		const headers = new Headers(init?.headers);
		seen.push({ url: String(url), key: headers.get('idempotency-key'), method: init?.method ?? 'GET' });
		const reply = replies[Math.min(seen.length - 1, replies.length - 1)]!;
		if (reply === 'refused') throw Object.assign(new TypeError('fetch failed'), { cause: { code: 'ECONNREFUSED' } });
		if (reply === 'reset') throw Object.assign(new TypeError('fetch failed'), { cause: { code: 'ECONNRESET' } });
		const response = new Response(JSON.stringify(reply.body ?? {}), { status: reply.status });
		if (reply.simulated !== false) response.headers.set('sandbox-simulated', 'true');
		response.headers.set('retry-after', '0');
		return response;
	}) as typeof globalThis.fetch;
	return { fetch, seen };
}

const busy = { status: 503, body: { error: { code: 'payment_busy', message: 'busy' }, simulated: true } };
const payment = { id: 'p1', status: 'authorizing', currency: 'USD', simulated: true };
const created = (state: string, status = 201) => ({
	status,
	body: { payment, operation: { id: 'o1', kind: 'authorize', state, amount: 100 }, replayed: status === 200, simulated: true }
});

describe('retries', () => {
	it('retries busy answers under one key, then answers', async () => {
		const { fetch, seen } = fake([busy, busy, created('succeeded')]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		const result = await sandbox.payments.create({ amount: 100, currency: 'USD' });
		expect(result.attempts).toBe(3);
		expect(result.outcome).toBe('succeeded');
		expect(result.settled).toBe(true);
		expect(new Set(seen.map((s) => s.key)).size).toBe(1);
		expect(seen[0]!.key).toBe(result.idempotencyKey);
	});

	it('stops after maxAttempts with an honest unknown that names the key', async () => {
		const { fetch, seen } = fake(['reset']);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		const error = await sandbox.payments.capture('p1', 100, { idempotencyKey: 'order-9-capture' }).catch((e) => e);
		expect(error).toBeInstanceOf(UnknownOutcomeError);
		expect(error.idempotencyKey).toBe('order-9-capture');
		expect(error.message).toContain('never with a new key');
		expect(seen.length).toBe(DEFAULT_RETRY.maxAttempts);
		expect(seen.every((s) => s.key === 'order-9-capture')).toBe(true);
	});

	it('never retries a refusal', async () => {
		const { fetch, seen } = fake([{ status: 409, body: { error: { code: 'idempotency_key_conflict', message: 'reused' } } }]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		const error = await sandbox.payments.create({ amount: 100, currency: 'USD' }).catch((e) => e);
		expect(error).toBeInstanceOf(RefusedError);
		expect([error.status, error.code]).toEqual([409, 'idempotency_key_conflict']);
		expect(seen.length).toBe(1);
	});

	it('re-sends a reset only when it cannot have arrived', async () => {
		const refused = fake(['refused', 'refused', { status: 200, body: { project: {}, reset: { from_epoch: 1, to_epoch: 2, archived_payments: 0 } } }]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch: refused.fetch, retry: FAST });
		expect((await sandbox.resetDataset({ confirm: 'shop', epoch: 1 })).to_epoch).toBe(2);
		expect(refused.seen.length).toBe(3);
		const lost = fake(['reset']);
		const again = new Sandbox({ apiKey: KEY, fetch: lost.fetch, retry: FAST });
		await expect(again.resetDataset({ confirm: 'shop', epoch: 1 })).rejects.toBeInstanceOf(UnreachableError);
		expect(lost.seen.length).toBe(1);
	});

	// v1 review L2: a 503 from inside the handler may follow a commit: a POST without a key
	// is never re-sent after it; only a shed request (503 overloaded) is.
	it('never re-sends a reset after an answer that may follow a commit', async () => {
		const done = { status: 200, body: { project: {}, reset: { from_epoch: 1, to_epoch: 2, archived_payments: 0 } } };
		for (const code of ['deadline_exceeded', 'unavailable']) {
			const late = fake([{ status: 503, body: { error: { code, message: 'ran out of time' } } }, done]);
			const sandbox = new Sandbox({ apiKey: KEY, fetch: late.fetch, retry: FAST });
			const error = await sandbox.resetDataset({ confirm: 'shop', epoch: 1 }).catch((e) => e);
			expect(error).toBeInstanceOf(UnreachableError);
			expect(error.message).toContain('may or may not');
			expect(late.seen.length).toBe(1);
		}
		const shed = fake([{ status: 503, body: { error: { code: 'overloaded', message: 'busy' } } }, done]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch: shed.fetch, retry: FAST });
		expect((await sandbox.resetDataset({ confirm: 'shop', epoch: 1 })).to_epoch).toBe(2);
		expect(shed.seen.length).toBe(2);
	});

	it('stops at once when the answer is not from a sandbox', async () => {
		const { fetch } = fake([{ status: 200, body: { ok: true }, simulated: false }]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		await expect(sandbox.project()).rejects.toBeInstanceOf(NotASandboxError);
	});

	// D-075: on a money-moving call, a server error or a proxy's answer for the sandbox is no
	// answer: retried under the same key, then unknown (settled: false), never a refusal.
	const internal = { status: 500, body: { error: { code: 'internal', message: 'internal error' }, simulated: true } };
	const gateways: Array<[string, Reply]> = [
		['500 from the sandbox', internal],
		['502 from a proxy', { status: 502, body: '', simulated: false }],
		['503 from a proxy', { status: 503, body: '', simulated: false }],
		['504 from a proxy', { status: 504, body: '', simulated: false }]
	];
	for (const [label, reply] of gateways) {
		it(`treats a ${label} on a money-moving call as no answer`, async () => {
			const once = fake([reply, created('succeeded', 200)]);
			const sandbox = new Sandbox({ apiKey: KEY, fetch: once.fetch, retry: FAST });
			const result = await sandbox.payments.create({ amount: 100, currency: 'USD' });
			expect([result.attempts, result.replayed]).toEqual([2, true]);
			expect(new Set(once.seen.map((s) => s.key)).size).toBe(1);

			const always = fake([reply]);
			const again = new Sandbox({ apiKey: KEY, fetch: always.fetch, retry: FAST });
			const error = await again.payments.capture('p1', 100, { idempotencyKey: 'order-7-capture' }).catch((e) => e);
			expect(error).toBeInstanceOf(UnknownOutcomeError);
			expect([error.idempotencyKey, error.settled, error.attempts]).toEqual(['order-7-capture', false, DEFAULT_RETRY.maxAttempts]);
			expect(error.message).toContain('idempotencyKey order-7-capture');
			expect(always.seen.length).toBe(DEFAULT_RETRY.maxAttempts);
		});
	}

	it('retries a read through a proxy that answers 502, then says unreachable, not "not a sandbox"', async () => {
		const { fetch, seen } = fake([{ status: 502, body: '', simulated: false }]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		const error = await sandbox.project().catch((e) => e);
		expect(error).toBeInstanceOf(UnreachableError);
		expect(error.message).toContain('502');
		expect(seen.length).toBe(DEFAULT_RETRY.maxAttempts);
		// A 4xx without the header is still not a sandbox.
		const other = fake([{ status: 404, body: '', simulated: false }]);
		const strict = new Sandbox({ apiKey: KEY, fetch: other.fetch, retry: FAST });
		await expect(strict.payments.create({ amount: 100, currency: 'USD' })).rejects.toBeInstanceOf(NotASandboxError);
	});

	// v1 review L1: an attempt's deadline is capped by what is left of the call's.
	it('never overruns the overall deadline', async () => {
		const seen: string[] = [];
		const hang = ((url: string | URL, init?: RequestInit) => {
			seen.push(String(url));
			return new Promise<Response>((_, reject) => {
				init?.signal?.addEventListener('abort', () => reject(init.signal!.reason));
			});
		}) as typeof globalThis.fetch;
		const sandbox = new Sandbox({ apiKey: KEY, fetch: hang, retry: { ...FAST, deadlineMs: 300, attemptTimeoutMs: 10_000 } });
		const started = Date.now();
		const error = await sandbox.payments.create({ amount: 100, currency: 'USD' }).catch((e) => e);
		expect(error).toBeInstanceOf(UnknownOutcomeError);
		expect(Date.now() - started).toBeLessThan(1_500);
	}, 3_000);

	it('reports an uncertain answer as not settled, never as success', async () => {
		const { fetch } = fake([created('uncertain', 202)]);
		const sandbox = new Sandbox({ apiKey: KEY, fetch, retry: FAST });
		const result = await sandbox.payments.create({ amount: 100, currency: 'USD', scenario: 'lost_reply' });
		expect([result.status, result.outcome, result.settled]).toEqual([202, 'uncertain', false]);
	});
});

describe('the parts', () => {
	it('retries only try-again answers', () => {
		for (const [status, code] of [[503, 'payment_busy'], [503, 'overloaded'], [503, 'unavailable'], [503, 'deadline_exceeded'], [429, 'rate_limited']] as const) {
			expect(retryable(status, code)).toBe(true);
		}
		for (const [status, code] of [[409, 'idempotency_key_conflict'], [422, 'raw_card_data_rejected'], [500, 'internal'], [503, 'other']] as const) {
			expect(retryable(status, code)).toBe(false);
		}
	});

	it('jitters backoff within its bound and honours Retry-After', () => {
		for (let attempts = 1; attempts <= 20; attempts += 1) {
			const ceiling = Math.min(DEFAULT_RETRY.capMs, DEFAULT_RETRY.baseMs * 2 ** Math.min(attempts - 1, 16));
			for (let i = 0; i < 20; i += 1) expect(backoffMs(DEFAULT_RETRY, attempts, null)).toBeLessThanOrEqual(ceiling);
		}
		expect(backoffMs(DEFAULT_RETRY, 1, 1_000, () => 0)).toBe(1_000);
		expect(backoffMs(DEFAULT_RETRY, 1, 3_600_000, () => 0)).toBe(DEFAULT_RETRY.capMs);
	});

	it('makes unique keys without digits', () => {
		const keys = new Set(Array.from({ length: 1_000 }, newIdempotencyKey));
		expect(keys.size).toBe(1_000);
		for (const key of keys) expect(key).toMatch(/^sdk_[a-p]{32}$/);
	});
});
