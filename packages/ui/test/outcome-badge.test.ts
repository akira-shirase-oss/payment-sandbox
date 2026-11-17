// The badge shows the word and an icon in its tone's colours; it never clips the word.
import { flushSync, mount, unmount } from 'svelte';
import { afterEach, expect, test } from 'vitest';
import OutcomeBadge from '../src/lib/components/sandbox/outcome-badge.svelte';
import { checkoutOutcome, type Outcome } from '../src/lib/outcome.ts';

let mounted: Record<string, unknown> | undefined;
afterEach(() => {
	if (mounted) unmount(mounted);
	mounted = undefined;
	document.body.innerHTML = '';
});

function render(outcome: Outcome): HTMLElement {
	mounted = mount(OutcomeBadge, { target: document.body, props: { outcome } });
	flushSync();
	const badge = document.body.querySelector<HTMLElement>('[data-slot="outcome"]');
	if (!badge) throw new Error('no badge');
	return badge;
}

test('every checkout state renders its own word, tone and icon', () => {
	for (const state of ['open', 'processing', 'uncertain', 'declined', 'not_executed', 'authorized', 'paid']) {
		const outcome = checkoutOutcome(state);
		const badge = render(outcome);
		expect(badge.textContent?.trim()).toBe(outcome.word);
		expect(badge.dataset['tone']).toBe(outcome.tone);
		expect(badge.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
		expect(badge.className).toContain('whitespace-nowrap');
		expect(badge.className).toContain('shrink-0');
		expect(badge.className).not.toContain('overflow-hidden');
		expect(badge.className).not.toContain('truncate');
		unmount(mounted!);
		mounted = undefined;
	}
});

test('uncertain is dashed and never reads as paid', () => {
	const badge = render(checkoutOutcome('uncertain'));
	expect(badge.className).toContain('border-dashed');
	expect(badge.textContent).not.toMatch(/paid/i);
	expect(badge.getAttribute('style')).toBeNull();
});
