// What every state the sandbox reports means to a person, in one place, so the checkout and
// the dashboard can never disagree. Each state has a tone (its colour and icon) and a word;
// the tone is never the only signal. Anything this file does not know is "Unknown", in the
// uncertain tone: an unrecognised state is never shown as success.

/** A state's family: its colour, its icon. */
export type Tone = 'ok' | 'held' | 'progress' | 'uncertain' | 'fail' | 'none' | 'refund';

/** How one state is shown. */
export interface Outcome {
	tone: Tone;
	/** The word on the badge. Short, never truncated. */
	word: string;
	/** One plain sentence about what it means for the money. */
	detail: string;
}

const UNKNOWN: Outcome = {
	tone: 'uncertain',
	word: 'Unknown',
	detail: 'This page does not know this state; treat the money as not settled.'
};

function lookup(table: Record<string, Outcome>, state: string | null | undefined): Outcome {
	if (state === null || state === undefined) return UNKNOWN;
	return Object.hasOwn(table, state) ? (table[state] ?? UNKNOWN) : UNKNOWN;
}

const CHECKOUT: Record<string, Outcome> = {
	open: { tone: 'none', word: 'Awaiting payment', detail: 'Nothing has been submitted yet.' },
	processing: {
		tone: 'progress',
		word: 'Processing',
		detail: 'Submitted. Waiting for the simulated network to answer.'
	},
	uncertain: {
		tone: 'uncertain',
		word: 'Uncertain',
		detail:
			'The simulated network’s answer was lost. The sandbox is reading its record back; this page updates by itself. Do not pay again.'
	},
	declined: { tone: 'fail', word: 'Declined', detail: 'The simulated network declined. Nothing was charged.' },
	not_executed: {
		tone: 'none',
		word: 'Not charged',
		detail: 'The simulated network never executed it. Nothing was charged.'
	},
	authorized: {
		tone: 'held',
		word: 'Authorized',
		detail: 'A hold was placed on simulated funds; it is not captured.'
	},
	paid: { tone: 'ok', word: 'Paid', detail: 'The whole amount was captured (simulated money).' }
};

/** The client's own state when the checkout could not reach the sandbox at all. */
export const UNREACHABLE: Outcome = {
	tone: 'uncertain',
	word: 'Unknown',
	detail:
		'This page could not reach the sandbox, so it cannot tell whether the payment was submitted. Retrying is safe: it repeats the same request and can never charge twice.'
};

const OPERATION: Record<string, Outcome> = {
	pending: { tone: 'progress', word: 'Pending', detail: 'Recorded; the network has not answered.' },
	uncertain: {
		tone: 'uncertain',
		word: 'Uncertain',
		detail: 'The answer was lost; resolved only by reading the network’s record back.'
	},
	succeeded: { tone: 'ok', word: 'Succeeded', detail: 'Approved; its postings are in the journal.' },
	declined: { tone: 'fail', word: 'Declined', detail: 'Declined; any reservation was released.' },
	not_executed: {
		tone: 'none',
		word: 'Not executed',
		detail: 'Readback proved the network never executed it; any reservation was released.'
	}
};

const PAYMENT: Record<string, Outcome> = {
	authorizing: { tone: 'progress', word: 'Authorizing', detail: 'The authorization is pending or uncertain.' },
	declined: { tone: 'fail', word: 'Declined', detail: 'The authorization was declined.' },
	failed: { tone: 'none', word: 'Not executed', detail: 'The authorization never executed.' },
	authorized: { tone: 'held', word: 'Authorized', detail: 'Held; nothing captured yet.' },
	partially_captured: { tone: 'held', word: 'Part captured', detail: 'Some captured; more may be.' },
	captured: { tone: 'ok', word: 'Captured', detail: 'Captured; nothing more can be.' },
	voided: { tone: 'none', word: 'Voided', detail: 'Released without any capture.' },
	partially_refunded: { tone: 'refund', word: 'Part refunded', detail: 'Some captured money refunded.' },
	refunded: { tone: 'refund', word: 'Refunded', detail: 'Every captured unit refunded.' }
};

const DELIVERY: Record<string, Outcome> = {
	pending: { tone: 'progress', word: 'Pending', detail: 'Waiting for its next attempt.' },
	delivered: { tone: 'ok', word: 'Delivered', detail: 'The receiver answered 2xx.' },
	dead: { tone: 'fail', word: 'Dead', detail: 'Every attempt failed; nothing more will be sent.' }
};

/** A checkout session's payer-facing state. */
export const checkoutOutcome = (state: string | null | undefined): Outcome => lookup(CHECKOUT, state);
/** An operation's state. */
export const operationOutcome = (state: string | null | undefined): Outcome => lookup(OPERATION, state);
/** A payment's status. */
export const paymentOutcome = (status: string | null | undefined): Outcome => lookup(PAYMENT, status);
/** A webhook delivery's state. */
export const deliveryOutcome = (state: string | null | undefined): Outcome => lookup(DELIVERY, state);

/** A reconciliation report: clean, mismatched, or (a scan cut short) not known to be clean. */
export function reconciliationOutcome(clean: boolean | null | undefined, complete = true): Outcome {
	if (clean === true && complete) return { tone: 'ok', word: 'Clean', detail: 'Every balance equals its journal entries.' };
	if (clean === false && complete)
		return { tone: 'fail', word: 'Mismatch', detail: 'At least one figure disagrees with the journal.' };
	return { tone: 'uncertain', word: 'Incomplete', detail: 'The check did not finish; it is not known to be clean.' };
}
