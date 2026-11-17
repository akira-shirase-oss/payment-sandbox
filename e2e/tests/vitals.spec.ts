// Web vitals of the production builds (the checkout and the dashboard as `npm run build`
// makes them, served by the real server): LCP, CLS and INP for the checkout and the
// dashboard's main pages, each within the "good" thresholds (LCP ≤ 2.5 s, CLS ≤ 0.1,
// INP ≤ 200 ms). scripts/check.sh runs it through scripts/e2e.sh, so a regression fails the
// gate; scripts/web-perf.sh also records the numbers in perf/results/ (E2E_VITALS_OUT).
// Measured in the page with PerformanceObserver: LCP is the last largest-contentful-paint
// entry before the first input; CLS the largest session window of layout shifts without
// recent input; INP, with the one or two scripted interactions a page gets here, the longest
// event entry (entries under 16 ms are not reported, so 0 means "under 16 ms"). Loopback,
// Chromium headless: a development observation of the pages' own cost. SIMULATED MONEY ONLY.
import { writeFileSync } from 'node:fs';
import { expect, test, type Page } from '@playwright/test';
import { checkoutSession, env, project } from './support.js';

const BUDGET = { lcp_ms: 2_500, cls: 0.1, inp_ms: 200 };

const OBSERVE = `
	(() => {
		const v = (window.__vitals = { lcp: 0, cls: 0, inp: 0, inputs: 0 });
		let input = false;
		new PerformanceObserver((list) => {
			if (input) return;
			for (const e of list.getEntries()) v.lcp = e.startTime;
		}).observe({ type: 'largest-contentful-paint', buffered: true });
		let windowValue = 0, first = 0, last = 0;
		new PerformanceObserver((list) => {
			for (const e of list.getEntries()) {
				if (e.hadRecentInput) continue;
				if (windowValue > 0 && e.startTime - last < 1000 && e.startTime - first < 5000) {
					windowValue += e.value;
				} else {
					windowValue = e.value;
					first = e.startTime;
				}
				last = e.startTime;
				v.cls = Math.max(v.cls, windowValue);
			}
		}).observe({ type: 'layout-shift', buffered: true });
		new PerformanceObserver((list) => {
			for (const e of list.getEntries()) {
				if (!e.interactionId) continue;
				input = true;
				v.inputs += 1;
				v.inp = Math.max(v.inp, e.duration);
			}
		}).observe({ type: 'event', buffered: true, durationThreshold: 16 });
		addEventListener('pointerdown', () => (input = true), { capture: true });
		addEventListener('keydown', () => (input = true), { capture: true });
	})();`;

interface Vitals {
	lcp_ms: number;
	cls: number;
	inp_ms: number;
}

/** One page in a fresh context: load, settle, interact, then read the observers. */
async function measure(page: Page, url: string, ready: (page: Page) => Promise<void>, interact: (page: Page) => Promise<void>): Promise<Vitals> {
	await page.goto('about:blank');
	await page.goto(url);
	await ready(page);
	await page.waitForLoadState('networkidle');
	await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => setTimeout(resolve, 300))));
	await interact(page);
	await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => setTimeout(resolve, 300))));
	const v = await page.evaluate(() => (window as unknown as { __vitals: { lcp: number; cls: number; inp: number } }).__vitals);
	return { lcp_ms: Math.round(v.lcp), cls: Math.round(v.cls * 10_000) / 10_000, inp_ms: Math.round(v.inp) };
}

test('web vitals of the checkout and the dashboard stay within the good thresholds', async ({ browser, request }) => {
	test.setTimeout(120_000);
	const shop = await project(request, 'Vitals shop');
	const results: Record<string, Vitals> = {};
	const context = await browser.newContext();
	await context.addInitScript(OBSERVE);
	const page = await context.newPage();

	const session = await checkoutSession(request, shop.admin, { amount: 4_200, currency: 'USD', description: 'Order 3001' });
	results['checkout: open'] = await measure(
		page,
		session.url,
		async (p) => {
			await expect(p.getByTestId('amount')).toBeVisible();
		},
		async (p) => {
			await p.getByRole('radio', { name: /insufficient funds/ }).click();
			await p.getByRole('radio', { name: /^Approve/ }).click();
		}
	);

	// The dashboard: sign in once (a session cookie), then each main page.
	results['dashboard: sign in'] = await measure(
		page,
		env.dashboard,
		async (p) => {
			await expect(p.getByLabel('Sandbox key')).toBeVisible();
		},
		async (p) => {
			await p.getByLabel('Sandbox key').fill(shop.read);
			await p.getByRole('button', { name: 'Sign in' }).click();
			await expect(p.getByRole('heading', { name: 'Overview' })).toBeVisible();
		}
	);
	const paid = await request.post(`${env.api}/v1/payments`, {
		headers: { authorization: `Bearer ${shop.admin}`, 'idempotency-key': 'vitals-1' },
		data: { amount: 1_234, currency: 'EUR', token: 'tok_sim_approve' }
	});
	const paymentId = ((await paid.json()) as { payment: { id: string } }).payment.id;
	for (const [name, path] of [
		['overview', '/'],
		['payments', '/payments'],
		['payment', `/payments/${paymentId}`],
		['ledger', '/ledger'],
		['webhooks', '/webhooks']
	] as const) {
		results[`dashboard: ${name}`] = await measure(
			page,
			`${env.dashboard}${path}`,
			async (p) => {
				await expect(p.getByRole('heading', { level: 1 })).toBeVisible();
			},
			async (p) => {
				// The theme switch: on every page, and it repaints the whole page.
				const toggle = p.getByRole('button', { name: /theme|dark|light/i }).first();
				await toggle.click();
				await toggle.click();
			}
		);
	}
	await context.close();

	const out = process.env['E2E_VITALS_OUT'];
	if (out) {
		writeFileSync(
			out,
			JSON.stringify(
				{
					budget: BUDGET,
					method:
						'one visit per page in one browser context (production builds of both apps, served by the real server on loopback); LCP before the first input, CLS as the largest session window, INP as the longest event entry of the scripted interactions (entries under 16 ms are not reported: 0 means under 16 ms)',
					pages: results
				},
				null,
				2
			)
		);
	}
	console.log(JSON.stringify(results));
	const over = Object.entries(results).filter(
		([, v]) => v.lcp_ms > BUDGET.lcp_ms || v.cls > BUDGET.cls || v.inp_ms > BUDGET.inp_ms
	);
	expect(over, JSON.stringify(results)).toEqual([]);
	// Every page painted something large (an LCP of 0 would mean the measurement failed).
	expect(Object.values(results).every((v) => v.lcp_ms > 0), JSON.stringify(results)).toBe(true);
});
