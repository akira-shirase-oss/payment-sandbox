import { expect, test } from 'vitest';
import type { View } from './api.js';
import {
	attemptSettledWithoutMoney,
	newAttemptKey,
	pollDelay,
	safeReturnUrl,
	secretFromFragment,
	step
} from './flow.js';

function view(state: string, next: View['session']['next_action'], attempt = true): View {
	return {
		session: {
			id: '0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001',
			merchant: 'shop',
			amount: 1250,
			currency: 'USD',
			exponent: 2,
			amount_display: '12.50',
			description: null,
			capture_method: 'automatic',
			embed_origins: [],
			return_url: null,
			expires_at: 0,
			expired: false,
			attempts_left: 9,
			state,
			next_action: next
		},
		attempt: attempt ? { number: 1, scenario: 'tok_sim_approve', authorize: null, decline_code: null, capture: null } : null,
		captured: 0,
		refunded: 0,
		scenarios: [],
		simulated: true
	};
}

const SECRET = `cs_sim_${'abcdefghijklmnop'.repeat(4)}`;

test('only a secret of the exact shape is taken from the fragment', () => {
	expect(secretFromFragment(`#${SECRET}`)).toBe(SECRET);
	expect(secretFromFragment(SECRET)).toBe(SECRET);
	for (const bad of ['', '#', '#cs_sim_', `#${SECRET}x`, `#${SECRET.replace('a', '1')}`, '#sk_sim_abc', `#${SECRET}&x=1`]) {
		expect(secretFromFragment(bad)).toBeNull();
	}
});

test('a new attempt key is a UUID with a letter, re-rolled when it has none', () => {
	const allDigits = '12345678-1234-4123-8123-123456789012';
	const rolls = [allDigits, allDigits, '0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001'];
	expect(newAttemptKey(() => rolls.shift() ?? '')).toBe('0b1e6c8a-7f42-4c1e-9d3a-5be1f00d0001');
	expect(newAttemptKey()).toMatch(/^[0-9a-f-]{36}$/);
	expect(() => newAttemptKey(() => allDigits)).toThrow();
});

test('the next step follows the server, and only a settled attempt frees the key', () => {
	expect(step(view('open', 'pay', false))).toBe('pay');
	expect(step(view('uncertain', 'wait'))).toBe('wait');
	expect(step(view('authorized', 'continue'))).toBe('continue');
	expect(step(view('paid', null))).toBe('done');
	expect(attemptSettledWithoutMoney(view('open', 'pay', false))).toBe(true);
	expect(attemptSettledWithoutMoney(view('declined', 'pay'))).toBe(true);
	expect(attemptSettledWithoutMoney(view('not_executed', 'pay'))).toBe(true);
	for (const state of ['processing', 'uncertain', 'authorized', 'paid']) {
		expect(attemptSettledWithoutMoney(view(state, null))).toBe(false);
	}
});

test('polling backs off and return links must be http(s)', () => {
	expect(pollDelay(0)).toBe(1_000);
	expect(pollDelay(100)).toBe(3_000);
	expect(safeReturnUrl('http://127.0.0.1:56464/thanks')).toBe('http://127.0.0.1:56464/thanks');
	expect(safeReturnUrl('javascript:alert(1)')).toBeNull();
	expect(safeReturnUrl(' javascript:alert(1)')).toBeNull();
	expect(safeReturnUrl(null)).toBeNull();
});
