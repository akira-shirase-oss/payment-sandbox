// The payer's controls: radio buttons only (no field to type card data into), and a status
// that says "Paid" for the paid state alone.
import { flushSync, mount, unmount } from 'svelte';
import { afterEach, expect, test } from 'vitest';
import { UNREACHABLE } from '@sandbox/ui/outcome';
import type { Scenario, View } from './api.js';
import ScenarioPicker from './ScenarioPicker.svelte';
import Status from './Status.svelte';

let mounted: Record<string, unknown>[] = [];
afterEach(() => {
	for (const m of mounted) unmount(m);
	mounted = [];
	document.body.innerHTML = '';
});

const SCENARIOS: Scenario[] = [
	{ token: 'tok_sim_approve', label: 'Approve', kind: 'approve' },
	{ token: 'tok_sim_decline_do_not_honor', label: 'Decline: do not honor', kind: 'decline' },
	{ token: 'tok_sim_lost_reply', label: 'Uncertain: the network’s reply is lost', kind: 'uncertain' }
];

test('the scenario picker is radio buttons and nothing to type into', () => {
	mounted.push(mount(ScenarioPicker, { target: document.body, props: { scenarios: SCENARIOS, selected: 'tok_sim_approve' } }));
	flushSync();
	const inputs = [...document.body.querySelectorAll('input, textarea, select, [contenteditable]')];
	expect(inputs).toHaveLength(3);
	for (const input of inputs) {
		expect(input.getAttribute('type')).toBe('radio');
		expect(input.getAttribute('autocomplete')).toBeNull();
	}
	expect(document.body.querySelector<HTMLInputElement>('input[value="tok_sim_approve"]')?.checked).toBe(true);
	expect(document.body.textContent).toContain('What should the simulated network do?');
	// Nothing asks for card data, in words or in any field's name.
	expect(document.body.textContent).not.toMatch(/card|cvc|cvv|expiry/i);
	for (const input of inputs) expect(input.getAttribute('name')).toBe('scenario');
});

function view(state: string, decline: string | null = null): View {
	return {
		session: {
			id: 'x', merchant: 'shop', amount: 1250, currency: 'USD', exponent: 2, amount_display: '12.50',
			description: null, capture_method: 'automatic', embed_origins: [], return_url: null, expires_at: 0,
			expired: false, attempts_left: 9, state, next_action: null
		},
		attempt: { number: 1, scenario: 'tok_sim_approve', authorize: state, decline_code: decline, capture: null },
		captured: state === 'paid' ? 1250 : 0,
		refunded: 0,
		scenarios: SCENARIOS,
		simulated: true
	};
}

test('the status says what is true for every state, and paid only when paid', () => {
	for (const state of ['processing', 'uncertain', 'declined', 'not_executed', 'authorized', 'paid', 'something_new']) {
		const m = mount(Status, { target: document.body, props: { view: view(state, state === 'declined' ? 'insufficient_funds' : null) } });
		flushSync();
		const text = document.body.textContent ?? '';
		if (state === 'paid') expect(text).toContain('Paid');
		else expect(text).not.toMatch(/\bPaid\b/);
		if (state === 'declined') expect(text).toContain('insufficient funds');
		if (state === 'something_new') expect(text).toContain('Unknown');
		unmount(m);
		document.body.innerHTML = '';
	}
	mounted.push(mount(Status, { target: document.body, props: { view: view('processing'), override: UNREACHABLE } }));
	flushSync();
	expect(document.body.textContent).toContain('Unknown');
	expect(document.body.textContent).toContain('can never charge twice');
});
