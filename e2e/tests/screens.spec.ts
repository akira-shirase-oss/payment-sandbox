// Every screen and key state of both sites for docs/screens/ (the interface standard: 360 px
// and 1440 px, light and dark), reached the way a payer and a developer reach them. Runs only
// when E2E_SCREENS_DIR is set (scripts/screens.sh); CI writes no screenshots. The dataset
// page's states come from dataset.spec.ts. SIMULATED MONEY ONLY.
import { expect, test, type Page } from '@playwright/test';
import { checkoutSession, env, project, screens, statusOf } from './support.js';

test.skip(!process.env['E2E_SCREENS_DIR'], 'set E2E_SCREENS_DIR (scripts/screens.sh) to take screenshots');

const radio = (page: Page, name: RegExp) => page.getByRole('radio', { name });
const pay = (page: Page) => page.getByRole('button', { name: /^Pay / });

test('the checkout: open, paid, declined, uncertain, not executed, and invalid links', async ({ page, request }) => {
	test.setTimeout(180_000);
	const shop = await project(request, 'Screens shop');
	const session = async (amount: number, description: string) =>
		checkoutSession(request, shop.admin, { amount, currency: 'USD', description });

	const open = await session(4_200, 'Order 2001: a pair of simulated socks');
	await page.goto(open.url);
	await expect(page.getByTestId('amount')).toBeVisible();
	await screens(page, 'checkout-open');
	await radio(page, /^Approve/).check();
	await pay(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	await screens(page, 'checkout-paid');

	const declined = await session(1_500, 'Order 2002');
	await page.goto(declined.url);
	await radio(page, /insufficient funds/).check();
	await pay(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'declined');
	await screens(page, 'checkout-declined');

	const lost = await session(1_999, 'Order 2003');
	await page.goto(lost.url);
	await radio(page, /reply is lost/).check();
	await pay(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'uncertain');
	await screens(page, 'checkout-uncertain');
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid', { timeout: 30_000 });

	const unavailable = await session(2_500, 'Order 2004');
	await page.goto(unavailable.url);
	await radio(page, /unavailable/i).check();
	await pay(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'not_executed', { timeout: 30_000 });
	await screens(page, 'checkout-not-executed');

	// A link without its secret (of a session this browser never opened: an opened one keeps
	// its secret for reloads), and a link to a session that does not exist.
	const unopened = await session(100, 'Order 2005');
	await page.goto('about:blank');
	await page.goto(unopened.url.split('#')[0]!);
	await expect(page.getByRole('heading', { name: 'This checkout link is not valid' })).toBeVisible();
	await screens(page, 'checkout-invalid-link');
	await page.goto('about:blank');
	await page.goto(`${env.checkout}/c/019a0000-0000-7000-8000-000000000000#cs_sim_${'a'.repeat(64)}`);
	await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
	await screens(page, 'checkout-unknown-session');
});

test('the dashboard: sign-in, every section, a payment, an explanation and the error states', async ({ page, request }) => {
	test.setTimeout(180_000);
	const shop = await project(request, 'Screens shop');
	// Something to show: a paid checkout, a decline and a lost reply settled by readback.
	const paid = await checkoutSession(request, shop.admin, { amount: 4_200, currency: 'USD', description: 'Order 2101' });
	await page.goto(paid.url);
	await radio(page, /^Approve/).check();
	await pay(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	for (const [n, token] of [
		[1, 'tok_sim_decline_do_not_honor'],
		[2, 'tok_sim_approve'],
		[3, 'tok_sim_lost_reply']
	] as const) {
		const created = await request.post(`${env.api}/v1/payments`, {
			headers: { authorization: `Bearer ${shop.admin}`, 'idempotency-key': `screens-${n}` },
			data: { amount: 1_000 * n, currency: 'EUR', token, description: `Order 22${n}0` }
		});
		expect(created.status()).toBeLessThan(300);
	}
	const hook = await request.post(`${env.api}/v1/webhook_endpoints`, {
		headers: { authorization: `Bearer ${shop.admin}` },
		data: { url: `${env.store}/webhooks` }
	});
	expect(hook.status()).toBe(201);

	await page.goto(env.dashboard);
	await expect(page.getByLabel('Sandbox key')).toBeVisible();
	await screens(page, 'dashboard-sign-in');
	await page.getByLabel('Sandbox key').fill(`sk_sim_${'0'.repeat(64)}`);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByTestId('sign-in-error')).toBeVisible();
	await screens(page, 'dashboard-sign-in-refused');
	await page.getByLabel('Sandbox key').fill(shop.admin);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
	await page.waitForLoadState('networkidle');
	await screens(page, 'dashboard-overview');

	for (const [name, path] of [
		['payments', '/payments'],
		['ledger', '/ledger'],
		['explanation', '/ledger/USD/merchant_pending'],
		['webhooks', '/webhooks'],
		['simulator', '/simulator']
	] as const) {
		await page.goto(`${env.dashboard}${path}`);
		await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
		await page.waitForLoadState('networkidle');
		await screens(page, `dashboard-${name}`);
	}
	await page.goto(`${env.dashboard}/payments`);
	await page.getByRole('link', { name: /42\.00 USD/ }).first().click();
	await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
	await page.waitForLoadState('networkidle');
	await screens(page, 'dashboard-payment-paid');
	await page.goto(`${env.dashboard}/payments/019a0000-0000-7000-8000-000000000000`);
	await expect(page.getByTestId('load-error')).toBeVisible();
	await screens(page, 'dashboard-payment-not-found');
});
