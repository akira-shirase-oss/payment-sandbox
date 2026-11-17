// The checkout origin's own API (D-060). The page holds only the session's client secret;
// it sends it as a bearer token, never a cookie. A failure that leaves the outcome unknown
// (no answer, a timeout, a 5xx) is `Unreachable`: the page then offers a retry, which repeats
// the same attempt under the same Idempotency-Key and can never charge twice.

export interface Scenario {
	token: string;
	label: string;
	kind: 'approve' | 'decline' | 'uncertain';
}

export interface SessionView {
	id: string;
	merchant: string;
	amount: number;
	currency: string;
	exponent: number;
	amount_display: string;
	description: string | null;
	capture_method: 'automatic' | 'manual';
	embed_origins: string[];
	return_url: string | null;
	expires_at: number;
	expired: boolean;
	attempts_left: number;
	state: string;
	next_action: 'pay' | 'wait' | 'continue' | null;
}

export interface AttemptView {
	number: number;
	scenario: string;
	authorize: string | null;
	decline_code: string | null;
	capture: string | null;
}

export interface View {
	session: SessionView;
	attempt: AttemptView | null;
	captured: number;
	refunded: number;
	scenarios: Scenario[];
	simulated: true;
}

/** The sandbox answered with a refusal (4xx): nothing happened that a retry would change. */
export class Refused extends Error {
	constructor(
		readonly status: number,
		readonly code: string,
		message: string
	) {
		super(message);
	}
}

/** No usable answer: the request may or may not have been processed. */
export class Unreachable extends Error {}

/** Longest a call waits before the outcome is "unknown". */
export const TIMEOUT_MS = 15_000;

async function call(path: string, init: RequestInit): Promise<View> {
	let response: Response;
	try {
		response = await fetch(path, {
			...init,
			credentials: 'omit',
			cache: 'no-store',
			redirect: 'error',
			signal: AbortSignal.timeout(TIMEOUT_MS)
		});
	} catch {
		throw new Unreachable('no answer from the sandbox');
	}
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		throw new Unreachable(`an unreadable answer (${response.status})`);
	}
	if (response.status >= 500) throw new Unreachable(`the sandbox was busy (${response.status})`);
	if (!response.ok) {
		const error = (body as { error?: { code?: string; message?: string } }).error;
		throw new Refused(response.status, error?.code ?? 'refused', error?.message ?? 'refused');
	}
	return body as View;
}

const base = (id: string) => `/api/sessions/${encodeURIComponent(id)}`;
const auth = (secret: string) => ({ authorization: `Bearer ${secret}` });

/** The session as the payer may see it. */
export function getView(id: string, secret: string): Promise<View> {
	return call(base(id), { method: 'GET', headers: auth(secret) });
}

/** Starts (or, with the same key, repeats) an attempt with `token`. */
export function pay(id: string, secret: string, key: string, token: string): Promise<View> {
	return call(`${base(id)}/attempts`, {
		method: 'POST',
		headers: { ...auth(secret), 'content-type': 'application/json', 'idempotency-key': key },
		body: JSON.stringify({ token })
	});
}

/** Continues the latest attempt (its capture, after the network's answer was confirmed). */
export function resume(id: string, secret: string): Promise<View> {
	return call(`${base(id)}/continue`, {
		method: 'POST',
		headers: { ...auth(secret), 'content-type': 'application/json' },
		body: '{}'
	});
}
