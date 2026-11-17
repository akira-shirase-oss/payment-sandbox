// Webhook verification agrees with the Rust signer byte for byte (the shared vectors), and
// refuses forged, stale, tampered and unsimulated deliveries (D-020).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { WebhookVerificationError, signWebhook, verifyWebhook } from '../../src/index.ts';

const vectors = JSON.parse(readFileSync(new URL('../../../../fixtures/webhook-vectors.json', import.meta.url), 'utf8')) as {
	secrets: [string, string];
	event_id: string;
	timestamp: number;
	body: string;
	signature_header_one_secret: string;
	signature_header_rotation: string;
};

const headers = (signature: string, eventId = vectors.event_id) => ({ 'Sandbox-Signature': signature, 'sandbox-event-id': eventId });
const code = (fn: () => unknown) => {
	try {
		fn();
	} catch (error) {
		expect(error).toBeInstanceOf(WebhookVerificationError);
		return (error as WebhookVerificationError).code;
	}
	return 'accepted';
};

describe('webhook verification', () => {
	it('signs exactly as the server does', () => {
		const [a, b] = vectors.secrets;
		const t = vectors.timestamp;
		expect(`t=${t},v1=${signWebhook(a, vectors.event_id, t, vectors.body)}`).toBe(vectors.signature_header_one_secret);
		expect(`t=${t},v1=${signWebhook(a, vectors.event_id, t, vectors.body)},v1=${signWebhook(b, vectors.event_id, t, vectors.body)}`).toBe(
			vectors.signature_header_rotation
		);
	});

	it('accepts the server’s deliveries, with either secret during a rotation', () => {
		const now = vectors.timestamp + 10;
		const event = verifyWebhook({ body: vectors.body, headers: headers(vectors.signature_header_one_secret), secrets: [vectors.secrets[0]], now });
		expect([event.type, event.sequence, event.simulated]).toEqual(['capture.succeeded', 2, true]);
		const bytes = new TextEncoder().encode(vectors.body);
		expect(verifyWebhook({ body: bytes, headers: new Headers(headers(vectors.signature_header_rotation)), secrets: [vectors.secrets[1]], now }).id).toBe(
			vectors.event_id
		);
	});

	it('refuses forged, stale, tampered, mislabelled and malformed deliveries', () => {
		const now = vectors.timestamp;
		const signed = headers(vectors.signature_header_one_secret);
		expect(code(() => verifyWebhook({ body: vectors.body, headers: signed, secrets: [`whsec_sim_${'00'.repeat(32)}`], now }))).toBe('mismatch');
		expect(code(() => verifyWebhook({ body: vectors.body.replace('"sequence":2', '"sequence":3'), headers: signed, secrets: [vectors.secrets[0]], now }))).toBe(
			'mismatch'
		);
		expect(code(() => verifyWebhook({ body: vectors.body, headers: signed, secrets: [vectors.secrets[0]], now: now + 301 }))).toBe('stale');
		expect(code(() => verifyWebhook({ body: vectors.body, headers: signed, secrets: [vectors.secrets[0]], now: now - 301 }))).toBe('stale');
		expect(
			code(() => verifyWebhook({ body: vectors.body, headers: headers(vectors.signature_header_one_secret, 'another'), secrets: [vectors.secrets[0]], now }))
		).toBe('mismatch');
		expect(code(() => verifyWebhook({ body: vectors.body, headers: { 'sandbox-event-id': vectors.event_id }, secrets: [vectors.secrets[0]], now }))).toBe(
			'malformed'
		);
		expect(code(() => verifyWebhook({ body: vectors.body, headers: headers('t=1,v2=abc'), secrets: [vectors.secrets[0]], now }))).toBe('malformed');
		const unsimulated = vectors.body.replace('"simulated":true', '"simulated":false');
		const t = vectors.timestamp;
		const forged = `t=${t},v1=${signWebhook(vectors.secrets[0], vectors.event_id, t, unsimulated)}`;
		expect(code(() => verifyWebhook({ body: unsimulated, headers: headers(forged), secrets: [vectors.secrets[0]], now }))).toBe('not_simulated');
	});
});
