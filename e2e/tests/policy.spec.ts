// Both sites under their policies (D-061): the headers as served, light and dark mode, and
// zero violations while the pages run. SIMULATED MONEY ONLY.
import { expect, test } from '@playwright/test';
import { checkoutSession, env, project, statusOf, watchPolicy } from './support.js';

for (const scheme of ['light', 'dark'] as const) {
	test(`${scheme} mode runs under the policies with zero violations`, async ({ page, request }) => {
		await page.emulateMedia({ colorScheme: scheme });
		const shop = await project(request);
		const session = await checkoutSession(request, shop.admin, { amount: 1_000, currency: 'USD' });
		const violations = await watchPolicy(page);

		const response = await page.goto(session.url);
		const csp = response?.headers()['content-security-policy'] ?? '';
		expect(csp).toContain("script-src 'self' 'sha256-");
		// Styles: this origin, plus exactly the static style attributes the app inserts (by hash).
		expect(csp).toMatch(/style-src 'self'( 'unsafe-hashes'( 'sha256-[A-Za-z0-9+\/=]+')+)?;/);
		expect(csp).not.toContain('unsafe-inline');
		expect(csp).toContain("frame-ancestors 'none'");
		await expect(page.getByTestId('amount')).toBeVisible();
		await expect(page.locator('html')).toHaveClass(scheme === 'dark' ? /dark/ : /^(?!.*dark).*$/);
		const ground = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
		// Switch with the toggle, then back: both themes render, and each is remembered.
		await page.getByRole('button', { name: /Switch to (dark|light) mode/ }).click();
		await expect(page.locator('html')).toHaveClass(scheme === 'dark' ? /^(?!.*dark).*$/ : /dark/);
		const switched = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
		expect(switched).not.toBe(ground);
		await page.reload();
		await expect(page.locator('html')).toHaveClass(scheme === 'dark' ? /^(?!.*dark).*$/ : /dark/);
		await page.getByRole('button', { name: /Switch to (dark|light) mode/ }).click();
		await page.getByRole('radio', { name: /^Approve/ }).check();
		await page.getByRole('button', { name: /^Pay / }).click();
		await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');

		const dash = await page.goto(env.dashboard);
		const dashCsp = dash?.headers()['content-security-policy'] ?? '';
		expect(dashCsp).toContain("frame-ancestors 'none'");
		expect(dash?.headers()['x-frame-options']).toBe('DENY');
		await page.getByLabel('Sandbox key').fill(shop.read);
		await page.getByRole('button', { name: 'Sign in' }).click();
		await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
		for (const section of ['Payments', 'Ledger', 'Webhooks', 'Simulator', 'Overview']) {
			await page.getByRole('link', { name: section, exact: true }).click();
			await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
		}
		await page.getByRole('link', { name: 'Payments', exact: true }).click();
		await page.getByRole('link', { name: /10\.00 USD/ }).first().click();
		await expect(page.locator('main [data-slot="outcome"]').first()).toHaveText('Captured');
		expect(await violations()).toEqual([]);
	});
}
