// The dashboard journeys (ROADMAP M6): sign-in with a sandbox key into a host-only session,
// an explanation paged until its lines sum to the balance, and the simulator's controls
// behind the operator's credential. SIMULATED MONEY ONLY.
import { expect, test } from '@playwright/test';
import { env, project, watchPolicy } from './support.js';

async function pay(request: import('@playwright/test').APIRequestContext, key: string, n: number, amount: number) {
	const auth = await request.post(`${env.api}/v1/payments`, {
		headers: { authorization: `Bearer ${key}`, 'idempotency-key': `e2e-auth-${n}` },
		data: { amount, currency: 'USD', token: 'tok_sim_approve' }
	});
	expect(auth.status()).toBe(201);
	const id = (await auth.json()).payment.id;
	const capture = await request.post(`${env.api}/v1/payments/${id}/captures`, {
		headers: { authorization: `Bearer ${key}`, 'idempotency-key': `e2e-cap-${n}` },
		data: { amount }
	});
	expect(capture.status()).toBe(201);
}

async function signIn(page: import('@playwright/test').Page, key: string) {
	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill(key);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
}

test('signing in gives a host-only session cookie, and signing out ends it on the server', async ({ page, context, request }) => {
	const shop = await project(request);
	const violations = await watchPolicy(page);
	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill('sk_sim_not-a-key');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('alert')).toContainText('missing or invalid sandbox key');
	await page.getByLabel('Sandbox key').fill(shop.read);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
	const cookies = await context.cookies(env.dashboard);
	const session = cookies.find((c) => c.name === '__Host-ps_dashboard');
	expect(session).toBeDefined();
	expect(session).toMatchObject({ httpOnly: true, secure: true, sameSite: 'Strict', path: '/' });
	// The page cannot read it.
	expect(await page.evaluate(() => document.cookie)).not.toContain('ps_dashboard');
	// A state change without the CSRF token is refused, even with the cookie.
	const forged = await page.evaluate(async () => (await fetch('/api/session/sign-out', { method: 'POST', body: '{}', headers: { 'content-type': 'application/json' } })).status);
	expect(forged).toBe(403);
	await page.getByRole('button', { name: 'Sign out' }).click();
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
	const after = await page.evaluate(async () => (await fetch('/api/v1/payments')).status);
	expect(after).toBe(401);
	expect(await violations()).toEqual([]);
});

test('an explanation pages through the journal until its lines sum to the balance', async ({ page, request }) => {
	const shop = await project(request);
	for (let n = 0; n < 7; n += 1) await pay(request, shop.admin, n, 100 + n);
	await signIn(page, shop.read);
	await page.getByRole('link', { name: 'Ledger' }).click();
	await expect(page.getByRole('link', { name: 'merchant pending' }).first()).toBeVisible();
	// Pages of three lines, so the walk takes several pages.
	await page.goto(`${env.dashboard}/ledger/USD/merchant_pending?limit=3`);
	const summary = page.getByTestId('explain-summary');
	await expect(summary).toContainText('in 1 page');
	await expect(summary.locator('[data-slot="outcome"]')).toHaveText('Partly explained');
	let pages = 1;
	while (await page.getByRole('button', { name: 'Load the next page' }).isVisible()) {
		await page.getByRole('button', { name: 'Load the next page' }).click();
		pages += 1;
		await expect(summary).toContainText(`in ${pages} pages`);
	}
	expect(pages).toBeGreaterThanOrEqual(3);
	await expect(summary.locator('[data-slot="outcome"]')).toHaveText('Explained');
	const sum = await page.getByTestId('explained-sum').textContent();
	expect(sum).toBe(await page.getByTestId('journal-balance').textContent());
	await expect(page.getByTestId('virtual-rows')).toHaveAttribute('data-total', /\d+/);
});

test('the payments list stays bounded however many payments there are', async ({ page, request }) => {
	const shop = await project(request);
	for (let n = 0; n < 40; n += 1) {
		const auth = await request.post(`${env.api}/v1/payments`, {
			headers: { authorization: `Bearer ${shop.admin}`, 'idempotency-key': `e2e-list-${n}` },
			data: { amount: 1_000 + n, currency: 'EUR', token: 'tok_sim_decline_do_not_honor' }
		});
		expect(auth.status()).toBe(201);
	}
	await signIn(page, shop.read);
	await page.getByRole('link', { name: 'Payments' }).click();
	const list = page.getByTestId('virtual-rows');
	await expect(list).toHaveAttribute('data-total', '40');
	const rendered = Number(await list.getAttribute('data-rendered'));
	expect(rendered).toBeLessThan(40);
	await expect(list.getByRole('listitem').first()).toContainText('10.39 EUR');
	await list.evaluate((el) => el.scrollTo(0, el.scrollHeight));
	await expect(list.getByRole('listitem').last()).toContainText('10.00 EUR');
});

test('the simulator controls answer only the operator credential', async ({ page, request }) => {
	const shop = await project(request);
	await signIn(page, shop.admin);
	await page.getByRole('link', { name: 'Simulator' }).click();
	await page.getByLabel('Operator credential').fill('not-the-operator-credential-0123456789');
	await page.getByRole('button', { name: 'Read' }).click();
	await expect(page.getByRole('alert')).toContainText('operator credential');
	await page.getByLabel('Operator credential').fill(env.simControl);
	await page.getByRole('button', { name: 'Read' }).click();
	await expect(page.getByRole('status')).toHaveText('Clock read.');
	await expect(page.getByText(/Network time: \d{4}-/)).toBeVisible();
});
