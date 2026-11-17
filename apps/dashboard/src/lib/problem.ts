// Plain words for what went wrong, and what to do next (the interface standard: errors in
// plain words with a recovery; internal codes only behind a details toggle).
import { ApiError } from './api.js';

export interface Problem {
	title: string;
	words: string;
	/** The stable code and the server's own message, for the details toggle. */
	code: string;
	detail: string;
}

/** The shape of a sandbox key; anything else is refused before it is sent (D-068). */
export const KEY_SHAPE = /^sk_sim_[0-9a-f]{64}$/;

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** What went wrong loading `what` ("the ledger", "this payment"), in plain words. */
export function problem(error: unknown, what: string): Problem {
	const e = error instanceof ApiError ? error : new ApiError(0, 'failed', error instanceof Error ? error.message : 'Something went wrong.');
	const base = { code: e.code, detail: e.message };
	switch (e.code) {
		case 'not_found':
			return { ...base, title: `${capital(what)} was not found`, words: "It does not exist in this project's current dataset. Check the link, or go back to the list." };
		case 'dataset_archived':
			return {
				...base,
				title: `${capital(what)} belongs to an earlier dataset`,
				words: 'A reset archived it. It stays in the ledger, but the dashboard shows the current epoch only.'
			};
		case 'unreachable':
			return { ...base, title: 'The sandbox did not answer', words: 'Check that the server is running, then try again.' };
		case 'rate_limited':
			return { ...base, title: 'Too many reports at once', words: "This project has used its report budget for now. Wait a few seconds, then try again." };
		case 'forbidden':
			return { ...base, title: 'This key cannot see that', words: 'Sign out and sign in with a key that has the read scope or more.' };
		case 'overloaded':
		case 'unavailable':
		case 'deadline_exceeded':
			return { ...base, title: 'The sandbox is busy', words: 'It could not answer in time. Try again in a moment.' };
		default:
			return { ...base, title: `${capital(what)} could not be loaded`, words: `${e.message.replace(/\.$/, '')}. Try again; if it keeps failing, the server's log has the details.` };
	}
}

/** Why a sign-in failed, in plain words. */
export function signInProblem(error: unknown): Problem {
	const e = error instanceof ApiError ? error : new ApiError(0, 'failed', error instanceof Error ? error.message : 'Sign-in failed.');
	const base = { code: e.code, detail: e.message };
	if (e.code === 'unauthorized') {
		return {
			...base,
			title: 'That key was not accepted',
			words:
				"Check that you pasted the whole key. A revoked key stops working: create a new one with POST /v1/keys, or ask your sandbox's operator."
		};
	}
	if (e.code === 'not_a_sandbox_key') {
		return {
			...base,
			title: 'That is not a Payment Sandbox key',
			words:
				"It was not sent. Sandbox keys are sk_sim_ followed by 64 letters and digits (0-9, a-f); a real processor's key (sk_live_..., sk_test_...) never works here."
		};
	}
	return problem(e, 'the sign-in');
}
