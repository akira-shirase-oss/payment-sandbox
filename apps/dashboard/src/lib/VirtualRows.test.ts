// Only the rows in view (plus a margin) are in the document, however long the list grows.
import { flushSync, mount, unmount } from 'svelte';
import { afterEach, expect, test } from 'vitest';
import Harness from './VirtualRowsHarness.test.svelte';

let mounted: Record<string, unknown> | undefined;
afterEach(() => {
	if (mounted) unmount(mounted);
	mounted = undefined;
	document.body.innerHTML = '';
});

test('ten thousand rows render a bounded slice, and scrolling moves it', () => {
	const items = Array.from({ length: 10_000 }, (_, i) => `row ${i}`);
	mounted = mount(Harness, { target: document.body, props: { items } });
	flushSync();
	const list = document.body.querySelector<HTMLElement>('[data-testid="virtual-rows"]');
	if (!list) throw new Error('no list');
	expect(list.dataset['total']).toBe('10000');
	const rendered = () => document.body.querySelectorAll('[role="listitem"]').length;
	expect(rendered()).toBeLessThan(40);
	expect(document.body.textContent).toContain('row 0');
	expect(document.body.textContent).not.toContain('row 500');
	list.scrollTop = 500 * 40;
	list.dispatchEvent(new Event('scroll'));
	flushSync();
	expect(document.body.textContent).toContain('row 500');
	expect(document.body.textContent).not.toContain('row 0 ');
	expect(rendered()).toBeLessThan(40);
	// Sizes go through the CSSOM, never a style attribute in markup (the policy refuses those).
	expect(document.body.innerHTML).not.toMatch(/<[^>]+\sstyle="(?![^"]*(height|transform|max-height))/);
});
