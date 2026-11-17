// The dashboard origin's API (D-062). The session is a host-only cookie the browser sends by
// itself; every state change also carries the session's CSRF token. A 401 anywhere means
// the session ended (idle, sign-out, or its key was revoked).

export class ApiError extends Error {
	constructor(
		readonly status: number,
		readonly code: string,
		message: string
	) {
		super(message);
	}
}

/** Longest a call waits before it is reported as failed. */
export const TIMEOUT_MS = 15_000;

let csrf = '';
let onSignedOut: () => void = () => {};

/** Set by the session once it knows the token. */
export function useCsrf(token: string): void {
	csrf = token;
}

/** Called whenever the server says the session is gone. */
export function whenSignedOut(callback: () => void): void {
	onSignedOut = callback;
}

async function request<T>(method: 'GET' | 'POST', path: string, body?: unknown, headers: Record<string, string> = {}): Promise<T> {
	const init: RequestInit = {
		method,
		credentials: 'same-origin',
		cache: 'no-store',
		redirect: 'error',
		signal: AbortSignal.timeout(TIMEOUT_MS),
		headers: { ...headers }
	};
	if (method === 'POST') {
		init.headers = { ...headers, 'content-type': 'application/json', 'x-csrf-token': csrf };
		init.body = JSON.stringify(body ?? {});
	}
	let response: Response;
	try {
		response = await fetch(`/api${path}`, init);
	} catch {
		throw new ApiError(0, 'unreachable', 'The sandbox did not answer. Check that the server is running, then retry.');
	}
	const parsed = (await response.json().catch(() => null)) as
		| (T & { error?: { code?: string; message?: string } })
		| null;
	if (!response.ok) {
		// Only a missing or ended session signs out; a refused operator credential does not.
		if (response.status === 401 && parsed?.error?.code === 'unauthorized') onSignedOut();
		throw new ApiError(
			response.status,
			parsed?.error?.code ?? 'failed',
			parsed?.error?.message ?? `The sandbox answered ${response.status}.`
		);
	}
	if (parsed === null) throw new ApiError(response.status, 'unreadable', 'The sandbox sent an unreadable answer.');
	return parsed;
}

export const get = <T>(path: string, headers?: Record<string, string>) => request<T>('GET', path, undefined, headers);
export const post = <T>(path: string, body?: unknown, headers?: Record<string, string>) =>
	request<T>('POST', path, body, headers);

/** Signs in with a sandbox key, sent once as a bearer token; the answer sets the cookie. */
export async function signIn<T>(key: string): Promise<T> {
	let response: Response;
	try {
		response = await fetch('/api/session', {
			method: 'POST',
			credentials: 'same-origin',
			cache: 'no-store',
			redirect: 'error',
			signal: AbortSignal.timeout(TIMEOUT_MS),
			headers: { authorization: `Bearer ${key}` }
		});
	} catch {
		throw new ApiError(0, 'unreachable', 'The sandbox did not answer.');
	}
	const parsed = (await response.json().catch(() => null)) as (T & { error?: { code?: string; message?: string } }) | null;
	if (!response.ok || parsed === null) {
		throw new ApiError(response.status, parsed?.error?.code ?? 'failed', parsed?.error?.message ?? 'Sign-in failed.');
	}
	return parsed;
}

// The shapes the API answers with (only the fields the pages read).

export interface Payment {
	id: string;
	status: string;
	amount: number;
	currency: string;
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
	created_at: number;
}

export interface Operation {
	id: string;
	kind: string;
	state: string;
	amount: number;
	idempotency_key: string;
	decline_code: string | null;
	network_ref: string | null;
	attempts: number;
}

export interface Entry {
	id: string;
	seq: number;
	recipe: string;
	currency: string;
	posted_at: string;
	lines: [string, number][];
}

export interface EventRecord {
	id: string;
	kind: string;
	payment_id: string;
	sequence: number;
	created_at: number;
	deliveries: [string, string, number][];
}

export interface Balance {
	currency: string;
	account: string;
	normal_side: string;
	balance_minor: number;
	balance: string;
	signed_minor: number;
}

export interface ExplainedLine {
	seq: number;
	entry_id: string;
	recipe: string;
	payment_id: string | null;
	amount_minor: number;
	running_minor: number;
}

export interface Explanation {
	currency: string;
	account: string;
	balance_minor: number;
	balance: string;
	explanation: {
		balance_minor: number;
		line_count: number;
		lines: ExplainedLine[];
		next: { cursor: string } | null;
	};
}

export interface Endpoint {
	id: string;
	url: string;
	rotating: boolean;
	created_at: string;
	disabled_at: string | null;
	consecutive_failures: number;
	circuit_open_until: string | null;
}

export interface Report {
	clean: boolean;
	report: {
		imbalanced_currencies: [string, number][];
		payment_mismatches: string[];
		operation_entry_mismatches: number;
		account_mismatches: [string, string, number, number][];
		open_operations: number;
		payments_checked: number;
		entries_checked: number;
	};
	network?: { complete: boolean };
}

export interface TaskStatus {
	name: string;
	running: boolean;
	restarts: number;
	last_failure: string | null;
	since_beat_ms: number;
	stale: boolean;
}
