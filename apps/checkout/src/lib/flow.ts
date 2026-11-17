// The checkout page's decisions, kept pure so they are tested without a browser.

import type { View } from './api.js';

const SECRET = /^cs_sim_[a-p]{64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/** The client secret from a URL fragment (`#cs_sim_…`), if it has exactly that shape. */
export function secretFromFragment(hash: string): string | null {
	const candidate = hash.startsWith('#') ? hash.slice(1) : hash;
	return SECRET.test(candidate) ? candidate : null;
}

/** True for a session id the server could have issued. */
export const isSessionId = (id: string): boolean => UUID.test(id);

/**
 * A fresh Idempotency-Key for a new attempt: a UUID that contains a letter, because the
 * sandbox's card guard (D-043, D-059) treats an all-digit UUID like any other run of digits.
 */
export function newAttemptKey(random: () => string = () => crypto.randomUUID()): string {
	for (let i = 0; i < 16; i += 1) {
		const key = random();
		if (UUID.test(key) && /[a-f]/.test(key)) return key;
	}
	throw new Error('no usable random key');
}

/** What the page does after a view arrives. */
export type Step = 'pay' | 'wait' | 'continue' | 'done';

export function step(view: View): Step {
	switch (view.session.next_action) {
		case 'pay':
			return 'pay';
		case 'wait':
			return 'wait';
		case 'continue':
			return 'continue';
		default:
			return 'done';
	}
}

/** True when the latest attempt can no longer turn into money (or there is none): a new
 *  press of Pay gets a new key. */
export function attemptSettledWithoutMoney(view: View): boolean {
	const state = view.session.state;
	return view.attempt === null || state === 'declined' || state === 'not_executed' || state === 'open';
}

/** The wait before the next poll: 1 s, growing to 3 s. */
export const pollDelay = (polls: number): number => Math.min(1_000 + polls * 250, 3_000);

/** The return link, if the merchant gave one and it is plain http(s). */
export function safeReturnUrl(url: string | null): string | null {
	return url !== null && /^https?:\/\//i.test(url) ? url : null;
}

/** An attempt the page started and has not seen settle, kept for this tab only. */
export interface Pending {
	key: string;
	token: string;
}

const storageKey = (kind: 'secret' | 'pending', id: string) => `payment-sandbox.checkout.${kind}.${id}`;

function storage(): Storage | null {
	try {
		return window.sessionStorage;
	} catch {
		return null;
	}
}

/** Takes the secret from the fragment (and keeps it for a reload of this tab), or the one
 *  kept earlier. The fragment is removed from the address bar either way. */
export function takeSecret(id: string, hash: string): string | null {
	const store = storage();
	const fromHash = secretFromFragment(hash);
	if (hash) history.replaceState(history.state, '', location.pathname + location.search);
	if (fromHash) {
		store?.setItem(storageKey('secret', id), fromHash);
		return fromHash;
	}
	const kept = store?.getItem(storageKey('secret', id)) ?? null;
	return kept && secretFromFragment(kept) ? kept : null;
}

export function loadPending(id: string): Pending | null {
	try {
		const raw = storage()?.getItem(storageKey('pending', id));
		const parsed = raw ? (JSON.parse(raw) as Partial<Pending>) : null;
		return parsed && typeof parsed.key === 'string' && typeof parsed.token === 'string'
			? { key: parsed.key, token: parsed.token }
			: null;
	} catch {
		return null;
	}
}

export function savePending(id: string, pending: Pending): void {
	storage()?.setItem(storageKey('pending', id), JSON.stringify(pending));
}

export function clearPending(id: string): void {
	storage()?.removeItem(storageKey('pending', id));
}
