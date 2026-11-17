// Every state has a word and a tone, and nothing unknown or uncertain ever reads as paid.
import { expect, test } from 'vitest';
import {
	checkoutOutcome,
	deliveryOutcome,
	operationOutcome,
	paymentOutcome,
	reconciliationOutcome,
	UNREACHABLE
} from '../src/lib/outcome.ts';

const CHECKOUT_STATES = ['open', 'processing', 'uncertain', 'declined', 'not_executed', 'authorized', 'paid'];

test('each checkout state has its own word, and only paid says paid', () => {
	const words = CHECKOUT_STATES.map((s) => checkoutOutcome(s).word);
	expect(new Set(words).size).toBe(CHECKOUT_STATES.length);
	for (const state of CHECKOUT_STATES) {
		const outcome = checkoutOutcome(state);
		expect(outcome.word.length).toBeGreaterThan(0);
		expect(outcome.detail.length).toBeGreaterThan(0);
		if (state !== 'paid') {
			expect(outcome.word).not.toMatch(/paid/i);
			expect(outcome.tone).not.toBe('ok');
		}
	}
	expect(checkoutOutcome('paid')).toMatchObject({ tone: 'ok', word: 'Paid' });
	expect(checkoutOutcome('uncertain')).toMatchObject({ tone: 'uncertain', word: 'Uncertain' });
	expect(checkoutOutcome('uncertain').detail).toMatch(/Do not pay again/);
});

test('an unknown state, or none, is "Unknown" in the uncertain tone, never success', () => {
	for (const lookup of [checkoutOutcome, operationOutcome, paymentOutcome, deliveryOutcome]) {
		for (const state of ['', 'captured_twice', '__proto__', 'constructor', 'toString', null, undefined]) {
			expect(lookup(state)).toMatchObject({ tone: 'uncertain', word: 'Unknown' });
		}
	}
	expect(UNREACHABLE).toMatchObject({ tone: 'uncertain', word: 'Unknown' });
	expect(UNREACHABLE.detail).toMatch(/can never charge twice/);
});

test('operations, payments, deliveries and reports map to the right families', () => {
	expect(operationOutcome('succeeded').tone).toBe('ok');
	expect(operationOutcome('uncertain').tone).toBe('uncertain');
	expect(operationOutcome('not_executed').tone).toBe('none');
	expect(paymentOutcome('authorized').tone).toBe('held');
	expect(paymentOutcome('partially_refunded').tone).toBe('refund');
	expect(paymentOutcome('declined').tone).toBe('fail');
	expect(deliveryOutcome('dead').tone).toBe('fail');
	expect(reconciliationOutcome(true)).toMatchObject({ tone: 'ok', word: 'Clean' });
	expect(reconciliationOutcome(false)).toMatchObject({ tone: 'fail', word: 'Mismatch' });
	expect(reconciliationOutcome(true, false)).toMatchObject({ tone: 'uncertain', word: 'Incomplete' });
	expect(reconciliationOutcome(undefined).word).toBe('Incomplete');
});
