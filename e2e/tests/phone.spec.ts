// A 360 px phone (ROADMAP M6): both sites fit without sideways scrolling, and every outcome
// badge shows its whole word. SIMULATED MONEY ONLY.
import { expect, test } from '@playwright/test';
import { badgesWhole, checkoutSession, env, noSidewaysScroll, project, statusOf } from './support.js';

test.use({ viewport: { width: 360, height: 740 }, deviceScaleFactor: 2, hasTouch: true, isMobile: true });

test('at 360 px the checkout and the dashboard fit, with every badge whole', async ({ page, request }) => {
	const shop = await project(request, 'A shop with a rather long display name for a phone');
	const session = await checkoutSession(request, shop.admin, {
		amount: 123_456_789,
		currency: 'USD',
		description: 'An order with a long description that has to wrap on a narrow phone screen'
	});
	await page.goto(session.url);
	await expect(page.getByTestId('amount')).toContainText('1,234,567.89');
	await noSidewaysScroll(page);
	await badgesWhole(page);
	await page.getByRole('radio', { name: /reply is lost/ }).check();
	await page.getByRole('button', { name: /^Pay / }).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'uncertain');
	await badgesWhole(page);
	await noSidewaysScroll(page);
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid', { timeout: 30_000 });
	await badgesWhole(page);
	await page.screenshot({ path: test.info().outputPath('checkout-phone.png'), fullPage: true });

	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill(shop.read);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
	for (const section of ['Overview', 'Payments', 'Ledger', 'Webhooks']) {
		await page.getByRole('link', { name: section, exact: true }).click();
		await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
		await noSidewaysScroll(page);
		await badgesWhole(page);
		if (section === 'Ledger') {
			// The page's main number stays on screen beside its account (v1 review M2).
			await expect(page.getByTestId('balance').first()).toBeInViewport({ ratio: 1 });
		}
	}
	await page.getByRole('link', { name: 'Payments', exact: true }).click();
	await page.getByRole('link', { name: /1,234,567\.89 USD/ }).first().click();
	await expect(page.locator('main [data-slot="outcome"]').first()).toHaveText('Captured');
	await noSidewaysScroll(page);
	await badgesWhole(page);
	await page.screenshot({ path: test.info().outputPath('dashboard-phone.png'), fullPage: true });
});
