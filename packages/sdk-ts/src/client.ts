// The Payment Sandbox client (D-068). SIMULATED MONEY ONLY.
//
// - Every money-moving call carries an Idempotency-Key, and every retry reuses it: the
//   server replays or re-drives the same operation, so a retry can never charge twice.
// - Retries are bounded (attempts, jittered backoff, an overall deadline) and only for
//   answers that say "try again": no answer, 429, and 503 busy/overloaded/unavailable.
// - On a money-moving call, a server error (5xx) and a proxy's 5xx without the sandbox
//   header (the server restarting behind Caddy) are no answer too: the request may have been
//   applied, so it is retried under the same key, which the server replays or finishes,
//   never charging twice (D-075).
// - Answers are honest: a 202 uncertain operation comes back as `outcome: 'uncertain'`,
//   `settled: false`, never as success; no usable answer is UnknownOutcomeError
//   (`settled: false`), which names the key to retry with.
// - Every answer must say `Sandbox-Simulated: true`, or the client stops.

import { checkConfig, type CheckedConfig, type RetryPolicy, type SandboxOptions } from './config.ts';
import { NotASandboxError, RefusedError, SandboxError, UnknownOutcomeError, UnreachableError } from './errors.ts';

/** The label every sandbox carries. */
export const SANDBOX_NOTICE = 'SIMULATED MONEY ONLY: Payment Sandbox moves no real money and accepts no real cards.';

/** Largest answer read. */
const MAX_BODY_BYTES = 4 * 1024 * 1024;

export type Currency = 'USD' | 'EUR' | 'JPY' | 'KWD';
export type OperationState = 'pending' | 'uncertain' | 'succeeded' | 'declined' | 'not_executed';
/** What the simulated network does with an authorization (it maps to a `tok_sim_*` token). */
export type NetworkScenario =
	| 'approve'
	| 'decline_insufficient_funds'
	| 'decline_do_not_honor'
	| 'decline_expired'
	| 'lost_reply'
	| 'unavailable';

export interface Payment {
	id: string;
	status: string;
	amount: number;
	currency: Currency;
	token: string;
	description: string | null;
	authorized: number;
	captured: number;
	voided: number;
	refunded: number;
	capturable: number;
	refundable: number;
	reserved: { capture: number; void: number; refund: number };
	decline_code: string | null;
	authorize_operation_id: string;
	created_at: number;
	simulated: true;
}

export interface Operation {
	id: string;
	kind: 'authorize' | 'capture' | 'void' | 'refund';
	state: OperationState;
	amount: number;
	idempotency_key: string;
	decline_code: string | null;
	network_ref: string | null;
	attempts: number;
}

export interface Project {
	id: string;
	slug: string;
	name: string;
	dataset_epoch: number;
}

/** What a money-moving call did. Check `settled` before acting on `outcome`. */
export interface MoneyResult {
	/** The operation's state. `uncertain` and `pending` are not done: the network's answer is not known yet. */
	outcome: OperationState;
	/** False while the outcome is `pending` or `uncertain`; wait with `payments.waitForSettlement`. */
	settled: boolean;
	payment: Payment;
	operation: Operation;
	/** True when the sandbox answered from its record of an earlier request under the same key. */
	replayed: boolean;
	/** The key this call used; reuse it to retry this exact request. */
	idempotencyKey: string;
	/** HTTP status: 201 new, 200 replayed, 202 not settled yet. */
	status: number;
	/** Attempts the client made. */
	attempts: number;
}

export interface MoneyOptions {
	/** Reuse a key to retry a request whose answer you did not get; a fresh one by default. */
	idempotencyKey?: string;
}

interface Answer {
	status: number;
	body: Record<string, unknown>;
	attempts: number;
}

/** A new idempotency key: `sdk_` and 32 letters `a`–`p`. No digits, so the card guard can never mistake one. */
export function newIdempotencyKey(): string {
	const bytes = new Uint8Array(16);
	globalThis.crypto.getRandomValues(bytes);
	const letters = 'abcdefghijklmnop';
	let out = 'sdk_';
	for (const byte of bytes) out += letters[byte >> 4]! + letters[byte & 15]!;
	return out;
}

/** Answers that say "try again". */
export function retryable(status: number, code: string): boolean {
	if (status === 429) return true;
	return status === 503 && ['payment_busy', 'overloaded', 'unavailable', 'deadline_exceeded'].includes(code);
}

/** Try-again answers given before any handler ran, so nothing can have been applied. */
export function shedBeforeHandling(status: number, code: string): boolean {
	return status === 429 || (status === 503 && code === 'overloaded');
}

/** Full jitter over min(cap, base × 2^(attempts−1)), never below Retry-After (capped). */
export function backoffMs(policy: RetryPolicy, attempts: number, retryAfterMs: number | null, random = Math.random): number {
	const exponent = Math.min(Math.max(attempts - 1, 0), 16);
	const ceiling = Math.min(policy.capMs, policy.baseMs * 2 ** exponent);
	const jittered = Math.floor(random() * (ceiling + 1));
	return retryAfterMs === null ? jittered : Math.max(jittered, Math.min(retryAfterMs, policy.capMs));
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

async function readBounded(response: Response): Promise<Uint8Array> {
	if (!response.body) return new Uint8Array();
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_BODY_BYTES) {
			await reader.cancel();
			throw new Error('the answer exceeded 4 MiB');
		}
		chunks.push(value);
	}
	const out = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

/**
 * A 5xx without the sandbox header is a gateway (a proxy) answering for the sandbox: on a
 * read only 502–504, on a money-moving call any 5xx, since the request may have arrived.
 */
export function gatewayAnswer(status: number, keyed: boolean): boolean {
	return (status >= 502 && status <= 504) || (keyed && status >= 500 && status <= 599);
}

function refusedConnection(error: unknown): boolean {
	const cause = (error as { cause?: { code?: string } } | null)?.cause;
	return cause?.code === 'ECONNREFUSED';
}

/** The Payment Sandbox, for a test store's server. */
export class Sandbox {
	readonly config: CheckedConfig;
	readonly #fetch: typeof fetch;

	constructor(options: SandboxOptions) {
		this.config = checkConfig(options);
		this.#fetch = options.fetch ?? globalThis.fetch;
	}

	/** The server's own statement of what it is (`simulated: true`), checked. */
	async notice(): Promise<{ simulated: true; notice: string; ledger_semantics_version: number }> {
		const answer = await this.#send('GET', '/v1/sandbox');
		if (answer.body.simulated !== true) throw new NotASandboxError(this.config.apiUrl);
		return answer.body as { simulated: true; notice: string; ledger_semantics_version: number };
	}

	/** The key's project and its current dataset epoch. */
	async project(): Promise<Project> {
		return (await this.#send('GET', '/v1/project')).body.project as Project;
	}

	/**
	 * Opens a new, empty dataset epoch (D-066): needs an admin key, the project's slug as
	 * `confirm` and the current `epoch`. Nothing is deleted. A repeat of a reset that already
	 * happened is refused (`epoch_changed`), never applied twice. An `UnreachableError` means
	 * it may or may not have happened: `project()` tells (`dataset_epoch` is `epoch + 1` if it did).
	 */
	async resetDataset(input: { confirm: string; epoch: number }): Promise<{ project: Project; from_epoch: number; to_epoch: number; archived_payments: number }> {
		const body = (await this.#send('POST', '/v1/project/reset', input)).body;
		const reset = body.reset as { from_epoch: number; to_epoch: number; archived_payments: number };
		return { project: body.project as Project, ...reset };
	}

	readonly payments = {
		/** Authorizes `amount` minor units (a hold); `scenario` decides what the simulated network does. */
		create: (
			input: { amount: number; currency: Currency; scenario?: NetworkScenario; description?: string },
			options: MoneyOptions = {}
		): Promise<MoneyResult> =>
			this.#money(
				'/v1/payments',
				{
					amount: input.amount,
					currency: input.currency,
					token: `tok_sim_${input.scenario ?? 'approve'}`,
					...(input.description === undefined ? {} : { description: input.description })
				},
				options
			),
		/** Captures `amount` minor units of the hold. */
		capture: (paymentId: string, amount: number, options: MoneyOptions = {}) =>
			this.#money(`/v1/payments/${encodeURIComponent(paymentId)}/captures`, { amount }, options),
		/** Refunds `amount` minor units of captured money. */
		refund: (paymentId: string, amount: number, options: MoneyOptions = {}) =>
			this.#money(`/v1/payments/${encodeURIComponent(paymentId)}/refunds`, { amount }, options),
		/** Releases the uncaptured remainder. */
		void: (paymentId: string, options: MoneyOptions = {}) =>
			this.#money(`/v1/payments/${encodeURIComponent(paymentId)}/void`, {}, options),
		/** A payment and its operations. A payment a reset archived is `RefusedError` `dataset_archived` (410). */
		get: async (paymentId: string): Promise<{ payment: Payment; operations: Operation[] }> => {
			const body = (await this.#send('GET', `/v1/payments/${encodeURIComponent(paymentId)}`)).body;
			return { payment: body.payment as Payment, operations: body.operations as Operation[] };
		},
		/** The most recent payments, newest first. */
		list: async (options: { limit?: number; before?: string } = {}): Promise<{ payments: Payment[]; next_before: string | null }> => {
			const query = new URLSearchParams({ limit: String(options.limit ?? 50) });
			if (options.before) query.set('before', options.before);
			const body = (await this.#send('GET', `/v1/payments?${query}`)).body;
			return { payments: body.payments as Payment[], next_before: (body.next_before as string | null) ?? null };
		},
		/**
		 * Polls until none of the payment's operations is pending or uncertain (the reconciler
		 * reads the network's record back), or `timeoutMs` passes. `settled: false` then means
		 * exactly that: still not known, not failed.
		 */
		waitForSettlement: async (
			paymentId: string,
			options: { timeoutMs?: number } = {}
		): Promise<{ settled: boolean; payment: Payment; operations: Operation[] }> => {
			const deadline = Date.now() + (options.timeoutMs ?? 30_000);
			let pause = 100;
			for (;;) {
				const current = await this.payments.get(paymentId);
				const open = current.operations.some((op) => op.state === 'pending' || op.state === 'uncertain');
				if (!open) return { settled: true, ...current };
				if (Date.now() + pause > deadline) return { settled: false, ...current };
				await sleep(pause);
				pause = Math.min(pause * 2, 1_000);
			}
		}
	};

	/** Every account balance, summed from the journal. */
	async balances(): Promise<Array<{ currency: Currency; account: string; balance_minor: number; balance: string; signed_minor: number }>> {
		return (await this.#send('GET', '/v1/balances')).body.balances as Array<{
			currency: Currency;
			account: string;
			balance_minor: number;
			balance: string;
			signed_minor: number;
		}>;
	}

	/** The reconciliation report; `clean` is true only when nothing disagrees. */
	async reconciliation(options: { network?: boolean } = {}): Promise<{ clean: boolean; report: Record<string, unknown>; network?: Record<string, unknown> }> {
		const body = (await this.#send('GET', `/v1/reconciliation${options.network ? '?network=true' : ''}`)).body;
		return body as { clean: boolean; report: Record<string, unknown>; network?: Record<string, unknown> };
	}

	/** Events (one per resolved operation) with their delivery states. */
	async events(options: { paymentId?: string; limit?: number } = {}): Promise<Array<{ id: string; kind: string; payment_id: string; sequence: number }>> {
		const query = new URLSearchParams({ limit: String(options.limit ?? 100) });
		if (options.paymentId) query.set('payment_id', options.paymentId);
		return (await this.#send('GET', `/v1/events?${query}`)).body.events as Array<{ id: string; kind: string; payment_id: string; sequence: number }>;
	}

	async #money(path: string, body: Record<string, unknown>, options: MoneyOptions): Promise<MoneyResult> {
		const idempotencyKey = options.idempotencyKey ?? newIdempotencyKey();
		const answer = await this.#send('POST', path, body, idempotencyKey);
		const operation = answer.body.operation as Operation;
		return {
			outcome: operation.state,
			settled: operation.state !== 'pending' && operation.state !== 'uncertain',
			payment: answer.body.payment as Payment,
			operation,
			replayed: answer.body.replayed === true,
			idempotencyKey,
			status: answer.status,
			attempts: answer.attempts
		};
	}

	async #send(method: 'GET' | 'POST', path: string, body?: unknown, idempotencyKey?: string): Promise<Answer> {
		const { apiUrl, apiKey, retry } = this.config;
		const started = Date.now();
		let attempts = 0;
		let lastFailure = '';
		for (;;) {
			attempts += 1;
			let retryAfterMs: number | null = null;
			try {
				const headers: Record<string, string> = { authorization: `Bearer ${apiKey}`, accept: 'application/json' };
				if (idempotencyKey) headers['idempotency-key'] = idempotencyKey;
				if (body !== undefined) headers['content-type'] = 'application/json';
				const response = await this.#fetch(`${apiUrl}${path}`, {
					method,
					headers,
					body: body === undefined ? undefined : JSON.stringify(body),
					redirect: 'error',
					// One attempt never outlives the call's own deadline (v1 review L1).
					signal: AbortSignal.timeout(Math.max(1, Math.min(retry.attemptTimeoutMs, retry.deadlineMs - (Date.now() - started))))
				});
				const keyed = idempotencyKey !== undefined;
				if (response.headers.get('sandbox-simulated') !== 'true') {
					if (!gatewayAnswer(response.status, keyed)) throw new NotASandboxError(apiUrl);
					await response.body?.cancel();
					const gateway = `a proxy answered HTTP ${response.status} for the sandbox: the server behind it did not answer (restarting, or down)`;
					// A POST without a key may have arrived: never re-sent.
					if (method === 'POST' && !keyed) throw new UnreachableError(apiUrl, `${gateway}; the request may or may not have been applied`);
					lastFailure = gateway;
				} else {
					const bytes = await readBounded(response);
					// A server error on a money-moving call decides nothing by its body.
					const serverError = keyed && response.status >= 500;
					let parsed: Record<string, unknown> = {};
					if (bytes.byteLength > 0) {
						try {
							parsed = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
						} catch {
							if (!serverError) throw new SandboxError('unreadable', `The sandbox sent an unreadable answer (HTTP ${response.status}).`);
						}
					}
					if (response.ok) return { status: response.status, body: parsed, attempts };
					const envelope = parsed.error as { code?: string; message?: string; request_id?: string | null } | undefined;
					const refused = new RefusedError(
						response.status,
						envelope?.code ?? 'failed',
						envelope?.message ?? `The sandbox answered ${response.status}.`,
						envelope?.request_id ?? null
					);
					if (!serverError && !retryable(response.status, refused.code)) throw refused;
					// A POST without a key is re-sent only after an answer given before any handler
					// ran (429, 503 overloaded); another 503 may follow a commit (v1 review L2).
					if (method === 'POST' && !keyed && !shedBeforeHandling(response.status, refused.code)) {
						throw new UnreachableError(apiUrl, `${refused.message} (HTTP ${response.status}); the request may or may not have been applied`);
					}
					const after = Number(response.headers.get('retry-after'));
					retryAfterMs = Number.isFinite(after) && response.headers.get('retry-after') !== null ? after * 1_000 : null;
					lastFailure = serverError ? `${refused.message} (HTTP ${response.status})` : refused.message;
				}
			} catch (error) {
				if (error instanceof SandboxError) throw error;
				// No answer. A POST without a key is re-sent only if it cannot have arrived.
				if (method === 'POST' && !idempotencyKey && !refusedConnection(error)) {
					throw new UnreachableError(apiUrl, 'the connection failed after the request was sent');
				}
				lastFailure = refusedConnection(error) ? 'the connection was refused' : 'the connection failed or timed out';
			}
			const wait = backoffMs(retry, attempts, retryAfterMs);
			if (attempts >= retry.maxAttempts || Date.now() - started + wait > retry.deadlineMs) break;
			await sleep(wait);
		}
		if (idempotencyKey) throw new UnknownOutcomeError(idempotencyKey, attempts, lastFailure);
		throw new UnreachableError(apiUrl, lastFailure);
	}
}
