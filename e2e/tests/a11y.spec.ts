// axe on every screen of both sites, at 360 px and 1440 px, in light and dark mode: zero
// serious or critical WCAG 2.1 A/AA violations (the interface standard, M7; at 360 px since
// the v1 review's M2, which found scroll regions a keyboard could not reach there).
// SIMULATED MONEY ONLY.
import { expect, test, type Page } from '@playwright/test';
import { checkoutSession, env, project, seriousViolations, statusOf } from './support.js';

async function settle(page: Page) {
	// The pages load their data after the first paint; wait for the network to go quiet.
	await page.waitForLoadState('networkidle');
}

for (const width of [360, 1440] as const)
for (const scheme of ['light', 'dark'] as const) {
	test(`every screen is free of serious accessibility violations at ${width} px in ${scheme} mode`, async ({ page, request }) => {
		await page.setViewportSize({ width, height: width === 360 ? 780 : 900 });
		await page.emulateMedia({ colorScheme: scheme });
		const shop = await project(request, 'Accessible shop');
		const session = await checkoutSession(request, shop.admin, { amount: 2_500, currency: 'EUR', description: 'Order 7' });

		const found: Record<string, string[]> = {};
		const audit = async (name: string) => {
			await settle(page);
			const violations = await seriousViolations(page);
			if (violations.length > 0) found[name] = violations;
		};

		await page.goto(session.url);
		await expect(page.getByTestId('amount')).toBeVisible();
		await audit('checkout: open');
		await page.getByRole('radio', { name: /^Approve/ }).check();
		await page.getByRole('button', { name: /^Pay / }).click();
		await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
		await audit('checkout: paid');

		await page.goto(env.dashboard);
		await expect(page.getByLabel('Sandbox key')).toBeVisible();
		await audit('dashboard: sign in');
		await page.getByLabel('Sandbox key').fill(shop.admin);
		await page.getByRole('button', { name: 'Sign in' }).click();
		await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
		await audit('dashboard: overview');
		for (const [name, path] of [
			['payments', '/payments'],
			['ledger', '/ledger'],
			['explanation', '/ledger/EUR/payer_held'],
			['webhooks', '/webhooks'],
			['simulator', '/simulator'],
			['dataset', '/dataset']
		] as const) {
			await page.goto(`${env.dashboard}${path}`);
			await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
			await audit(`dashboard: ${name}`);
		}
		await page.goto(`${env.dashboard}/payments`);
		await page.locator('a[href^="/payments/"]').first().click();
		await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
		await audit('dashboard: payment');

		expect(found).toEqual({});
	});
}
