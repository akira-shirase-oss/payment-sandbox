// The page-load measurement (ROADMAP M6, perf/results): how long a first visit and a
// repeat visit take until the payer can act, on loopback against a debug or release build.
// Runs only when E2E_PERF_OUT names a file (scripts/web-perf.sh); a development
// observation, never capacity. SIMULATED MONEY ONLY.
import { writeFileSync } from 'node:fs';
import { expect, test, type Browser, type Page } from '@playwright/test';
import { checkoutSession, env, project } from './support.js';

const out = process.env['E2E_PERF_OUT'];
const SAMPLES = Number(process.env['E2E_PERF_SAMPLES'] ?? 25);

test.skip(!out, 'set E2E_PERF_OUT (scripts/web-perf.sh) to measure page loads');

interface Sample {
	ttfb: number;
	fcp: number;
	dom_content_loaded: number;
	load: number;
	ready: number;
	transferred_bytes: number;
}

/** Records when `selector` first appears, on the page's own clock (no round trip). */
const READY = (selector: string) => `
	(() => {
		const mark = () => { if (!window.__ready && document.querySelector(${JSON.stringify(selector)})) window.__ready = performance.now(); };
		new MutationObserver(mark).observe(document, { childList: true, subtree: true });
	})();`;

async function sample(page: Page, url: string, selector: string): Promise<Sample> {
	await page.goto(url);
	await expect(page.locator(selector).first()).toBeVisible();
	await page.waitForLoadState('load');
	return page.evaluate(() => {
		const nav = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming;
		const fcp = performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? -1;
		const resources = performance.getEntriesByType('resource') as PerformanceResourceTiming[];
		return {
			ttfb: nav.responseStart,
			fcp,
			dom_content_loaded: nav.domContentLoadedEventEnd,
			load: nav.loadEventEnd,
			ready: (window as unknown as { __ready?: number }).__ready ?? -1,
			transferred_bytes: nav.transferSize + resources.reduce((total, r) => total + r.transferSize, 0)
		};
	});
}

async function series(browser: Browser, url: () => Promise<string>, selector: string) {
	const cold: Sample[] = [];
	const warm: Sample[] = [];
	for (let i = 0; i < SAMPLES; i += 1) {
		const context = await browser.newContext();
		await context.addInitScript(READY(selector));
		const page = await context.newPage();
		const target = await url();
		cold.push(await sample(page, target, selector));
		warm.push(await sample(page, target, selector));
		await context.close();
	}
	return { cold: summarize(cold), warm: summarize(warm) };
}

function summarize(samples: Sample[]) {
	const keys = Object.keys(samples[0] ?? {}) as (keyof Sample)[];
	return Object.fromEntries(
		keys.map((key) => {
			const values = samples.map((s) => s[key]).sort((a, b) => a - b);
			const at = (q: number) => values[Math.min(values.length - 1, Math.floor(q * values.length))] ?? -1;
			return [key, { p50: round(at(0.5)), p95: round(at(0.95)), max: round(values[values.length - 1] ?? -1) }];
		})
	);
}

const round = (n: number) => Math.round(n * 10) / 10;

test('page loads: the checkout until Pay is on screen, the dashboard until Sign in is', async ({ browser, request }) => {
	test.setTimeout(10 * 60_000);
	const shop = await project(request);
	const checkout = await series(
		browser,
		async () => (await checkoutSession(request, shop.admin, { amount: 1_000, currency: 'USD' })).url,
		'button[type="submit"]'
	);
	const dashboard = await series(browser, async () => env.dashboard, 'button[type="submit"]');
	const result = {
		samples: SAMPLES,
		unit: 'milliseconds from navigation start (transferred_bytes: bytes)',
		note: 'loopback, Chromium headless, same machine as the servers: a development observation',
		checkout,
		dashboard
	};
	writeFileSync(out!, JSON.stringify(result, null, 2));
	console.log(JSON.stringify(result));
});
