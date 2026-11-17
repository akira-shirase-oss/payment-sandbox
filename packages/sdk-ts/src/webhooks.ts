// Verifying the sandbox's signed webhooks (D-020) in a test store's server.
// SIMULATED MONEY ONLY.
//
// The signature is HMAC-SHA256, keyed by the endpoint's signing secret (`whsec_sim_...`),
// over `{event id}.{unix seconds}.{raw body}`, sent as
// `Sandbox-Signature: t=<unix>,v1=<hex>[,v1=<hex>]` (two v1 values during a rotation), with
// the event id in `Sandbox-Event-Id`. Verify the raw bytes before parsing them, refuse
// timestamps more than 300 s away, and deduplicate by event id: deliveries are at least once.

import { createHmac, timingSafeEqual } from 'node:crypto';
import { WebhookVerificationError } from './errors.ts';

/** The thin event a delivery carries; re-read the payment for its current state (D-025). */
export interface SandboxEvent {
	id: string;
	/** `{operation kind}.{final state}`, e.g. `capture.succeeded`. */
	type: string;
	project_id: string;
	payment_id: string;
	operation_id: string;
	/** Per-payment order, from 1. */
	sequence: number;
	created_at: number;
	simulated: true;
}

export const SIGNATURE_HEADER = 'sandbox-signature';
export const EVENT_ID_HEADER = 'sandbox-event-id';
export const TOLERANCE_SECONDS = 300;

type HeaderBag = Headers | Record<string, string | string[] | undefined>;

function header(headers: HeaderBag, name: string): string | undefined {
	if (typeof (headers as Headers).get === 'function') return (headers as Headers).get(name) ?? undefined;
	const bag = headers as Record<string, string | string[] | undefined>;
	const key = Object.keys(bag).find((k) => k.toLowerCase() === name);
	const value = key === undefined ? undefined : bag[key];
	return Array.isArray(value) ? value[0] : value;
}

/** The hex `v1` signature of `body` under `secret`. */
export function signWebhook(secret: string, eventId: string, timestamp: number, body: string | Uint8Array): string {
	return createHmac('sha256', secret).update(`${eventId}.${timestamp}.`).update(body).digest('hex');
}

/**
 * Verifies one delivery and returns its event. Throws WebhookVerificationError
 * (`malformed`, `stale`, `mismatch`, `not_simulated`) otherwise: answer it with 401/400 and
 * the sandbox retries.
 */
export function verifyWebhook(input: {
	/** The raw request body, exactly as received. */
	body: string | Uint8Array;
	headers: HeaderBag;
	/** The endpoint's signing secrets (two during a rotation). */
	secrets: string[];
	/** Unix seconds now (for tests). */
	now?: number;
}): SandboxEvent {
	const signature = header(input.headers, SIGNATURE_HEADER);
	const eventId = header(input.headers, EVENT_ID_HEADER);
	if (!signature || !eventId) {
		throw new WebhookVerificationError('malformed', 'The delivery lacks the Sandbox-Signature or Sandbox-Event-Id header.');
	}
	let timestamp: number | null = null;
	const signatures: Buffer[] = [];
	for (const part of signature.split(',')) {
		const [name, value] = part.split('=', 2);
		if (name === 't' && value !== undefined && /^-?\d{1,15}$/.test(value)) timestamp = Number(value);
		else if (name === 'v1' && value !== undefined && /^[0-9a-f]{64}$/.test(value)) signatures.push(Buffer.from(value, 'hex'));
		else throw new WebhookVerificationError('malformed', 'The Sandbox-Signature header is malformed.');
	}
	if (timestamp === null || signatures.length === 0 || signatures.length > 4) {
		throw new WebhookVerificationError('malformed', 'The Sandbox-Signature header is malformed.');
	}
	const now = input.now ?? Math.floor(Date.now() / 1000);
	if (Math.abs(now - timestamp) > TOLERANCE_SECONDS) {
		throw new WebhookVerificationError('stale', 'The delivery is older (or newer) than 300 s: a replay, or a clock far off.');
	}
	const matches = input.secrets.some((secret) => {
		const expected = Buffer.from(signWebhook(secret, eventId, timestamp, input.body), 'hex');
		return signatures.some((given) => given.length === expected.length && timingSafeEqual(given, expected));
	});
	if (!matches) {
		throw new WebhookVerificationError('mismatch', 'No signature matches a signing secret: check the secret, or the body was changed.');
	}
	const text = typeof input.body === 'string' ? input.body : new TextDecoder().decode(input.body);
	let event: SandboxEvent;
	try {
		event = JSON.parse(text) as SandboxEvent;
	} catch {
		throw new WebhookVerificationError('malformed', 'The signed body is not JSON.');
	}
	if (event.id !== eventId) {
		throw new WebhookVerificationError('malformed', 'The body names another event than Sandbox-Event-Id.');
	}
	if (event.simulated !== true) {
		throw new WebhookVerificationError('not_simulated', 'The event is not marked simulated: never fulfill an order from it.');
	}
	return event;
}
