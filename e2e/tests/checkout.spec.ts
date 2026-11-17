// The checkout journeys (ROADMAP M6), in Chromium against the real processes: a purchase
// end to end through fulfillment, refund and the dashboard; a decline; an uncertain answer
// that is never shown as paid until the reconciler resolves it; a lost response retried
// without a second charge; the keyboard alone; no card field anywhere; embedding only where
// the session allows. SIMULATED MONEY ONLY.
import { expect, test, type Page } from '@playwright/test';
import {
	api,
	checkoutSession,
	env,
	focusVisible,
	merchantView,
	project,
	statusOf,
	tabTo,
	watchPolicy
} from './support.js';

const radio = (page: Page, name: RegExp) => page.getByRole('radio', { name });
const payButton = (page: Page) => page.getByRole('button', { name: /^Pay / });

/** Every state the status region showed, in order (sampled each animation frame). */
async function recordStates(page: Page) {
	await page.addInitScript(() => {
		const seen: string[] = [];
		(window as unknown as { __states: string[] }).__states = seen;
		const sample = () => {
			const state = document.querySelector('[data-testid="status"]')?.getAttribute('data-state');
			if (state && seen[seen.length - 1] !== state) seen.push(state);
			requestAnimationFrame(sample);
		};
		requestAnimationFrame(sample);
	});
	return () => page.evaluate(() => (window as unknown as { __states: string[] }).__states);
}

test('a purchase is paid, fulfilled by the dev store, partly refunded and balanced in the dashboard', async ({
	page,
	request
}) => {
	const shop = await project(request, 'Dev store');
	const endpoint = await request.post(`${env.api}/v1/webhook_endpoints`, {
		headers: { authorization: `Bearer ${shop.admin}` },
		data: { url: `${env.store}/webhooks` }
	});
	expect(endpoint.status(), await endpoint.text()).toBe(201);
	const configured = await request.put(`${env.store}/config`, {
		headers: { authorization: `Bearer ${env.storeToken}` },
		data: { sandbox_url: env.api, api_key: shop.read, secrets: [(await endpoint.json()).secret] }
	});
	expect(configured.status()).toBe(200);

	const session = await checkoutSession(request, shop.admin, {
		amount: 4_200,
		currency: 'USD',
		description: 'Order 1001: one sandbox mug'
	});
	const violations = await watchPolicy(page);
	await page.goto(session.url);
	await expect(page.getByTestId('amount')).toContainText('42.00');
	await expect(page.getByText('Order 1001: one sandbox mug')).toBeVisible();
	await expect(page.getByText('SIMULATED MONEY ONLY.')).toBeVisible();
	// The secret left the address bar (and was never sent in a URL).
	expect(page.url()).not.toContain('#');
	await radio(page, /^Approve/).check();
	await payButton(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	await expect(statusOf(page).getByText('Paid', { exact: true })).toBeVisible();

	const merchant = await merchantView(request, shop.admin, session.id);
	expect(merchant.attempts).toHaveLength(1);
	const paymentId: string = merchant.attempts[0].payment_id;
	// The durable webhook reaches the dev store, which fulfills the order once.
	await expect
		.poll(
			async () => {
				const listed = await (await request.get(`${env.store}/fulfillments`)).json();
				return (listed.fulfillments as { payment_id: string }[]).filter((f) => f.payment_id === paymentId).length;
			},
			{ timeout: 30_000 }
		)
		.toBe(1);
	// The merchant refunds part of it through the API.
	const refund = await request.post(`${env.api}/v1/payments/${paymentId}/refunds`, {
		headers: { authorization: `Bearer ${shop.admin}`, 'idempotency-key': `refund-${paymentId}` },
		data: { amount: 1_200 }
	});
	expect(refund.status(), await refund.text()).toBe(201);

	// The dashboard shows it, balanced.
	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill(shop.read);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
	await expect(page.getByTestId('reconciliation').getByText('Clean', { exact: true })).toBeVisible();
	await page.getByRole('link', { name: 'Payments' }).click();
	await page.getByRole('link', { name: /42\.00 USD/ }).first().click();
	await expect(page.getByRole('heading', { name: '42.00 USD' })).toBeVisible();
	await expect(page.locator('main [data-slot="outcome"]').first()).toHaveText('Part refunded');
	await expect(page.getByTestId('payment-sums')).toContainText('30.00 USD');
	for (const kind of ['authorize', 'capture', 'refund']) {
		await expect(page.getByRole('cell', { name: kind, exact: true })).toBeVisible();
	}
	// And the payer's page knows about the refund too, still saying "Paid".
	await page.goto(`${env.checkout}/c/${session.id}#${session.secret}`);
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	await expect(page.getByText('42.00 USD captured; 12.00 USD refunded since.')).toBeVisible();
	const report = await api(request, shop.admin, '/v1/reconciliation?network=true');
	expect(report.clean).toBe(true);
	expect(await violations()).toEqual([]);
});

test('a decline says so, charges nothing, and the payer can try again', async ({ page, request }) => {
	const shop = await project(request);
	const session = await checkoutSession(request, shop.admin, { amount: 900, currency: 'JPY' });
	await page.goto(session.url);
	await expect(page.getByTestId('amount')).toContainText('900');
	await radio(page, /insufficient funds/).check();
	await payButton(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'declined');
	await expect(statusOf(page).getByText('Declined', { exact: true })).toBeVisible();
	await expect(page.getByText('Reason: insufficient funds (simulated).')).toBeVisible();
	await expect(statusOf(page)).not.toContainText(/\bPaid\b/);
	// The picker is back; the second attempt approves.
	await radio(page, /^Approve/).check();
	await payButton(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	const merchant = await merchantView(request, shop.admin, session.id);
	expect(merchant.attempts).toHaveLength(2);
	const payments = await api(request, shop.admin, '/v1/payments');
	const statuses = (payments.payments as { status: string }[]).map((p) => p.status).sort();
	expect(statuses).toEqual(['captured', 'declined']);
});

test('an uncertain answer is shown as uncertain, never paid, until the reconciler resolves it', async ({
	page,
	request
}) => {
	const shop = await project(request);
	const session = await checkoutSession(request, shop.admin, { amount: 1_999, currency: 'EUR' });
	const states = await recordStates(page);
	await page.goto(session.url);
	await radio(page, /reply is lost/).check();
	await payButton(page).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'uncertain');
	await expect(statusOf(page).getByText('Uncertain', { exact: true })).toBeVisible();
	await expect(statusOf(page)).toContainText('Do not pay again');
	// No way to pay twice while the answer is unknown.
	await expect(payButton(page)).toHaveCount(0);
	// The reconciler reads the network's record back; the page continues and captures.
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid', { timeout: 30_000 });
	const seen = await states();
	expect(seen).toContain('uncertain');
	const firstPaid = seen.indexOf('paid');
	expect(seen.slice(0, firstPaid)).not.toContain('paid');
	expect(seen.lastIndexOf('uncertain')).toBeLessThan(firstPaid);
	const merchant = await merchantView(request, shop.admin, session.id);
	expect(merchant.attempts).toHaveLength(1);
	const payment = await api(request, shop.admin, `/v1/payments/${merchant.attempts[0].payment_id}`);
	expect(payment.payment.captured).toBe(1_999);
	expect((payment.operations as { kind: string }[]).map((o) => o.kind)).toEqual(['authorize', 'capture']);
	const report = await api(request, shop.admin, '/v1/reconciliation?network=true');
	expect(report.clean).toBe(true);
});

test('a lost response and a double click are retried without a second charge', async ({ page, request }) => {
	const shop = await project(request);
	const session = await checkoutSession(request, shop.admin, { amount: 2_500, currency: 'USD' });
	await page.goto(session.url);
	// The first answer is lost on its way back: the server processed it, the page never
	// heard. The browser sees a network failure.
	let dropped = 0;
	await page.route('**/api/sessions/*/attempts', async (route) => {
		if (dropped === 0) {
			dropped += 1;
			await route.fetch();
			await route.abort('connectionreset');
			return;
		}
		await route.continue();
	});
	await radio(page, /^Approve/).check();
	await payButton(page).dblclick();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'unknown');
	await expect(statusOf(page).getByText('Unknown', { exact: true })).toBeVisible();
	await expect(statusOf(page)).toContainText('can never charge twice');
	await page.getByRole('button', { name: 'Retry safely' }).click();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	// A reload keeps the same attempt: still paid, still one charge.
	await page.reload();
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
	expect(dropped).toBe(1);
	const merchant = await merchantView(request, shop.admin, session.id);
	expect(merchant.attempts).toHaveLength(1);
	const payments = await api(request, shop.admin, '/v1/payments');
	expect(payments.payments).toHaveLength(1);
	expect(payments.payments[0].captured).toBe(2_500);
	const report = await api(request, shop.admin, '/v1/reconciliation?network=true');
	expect(report.clean).toBe(true);
});

test('the checkout can be paid with the keyboard alone', async ({ page, request }) => {
	const shop = await project(request);
	const session = await checkoutSession(request, shop.admin, { amount: 700, currency: 'KWD' });
	await page.goto(session.url);
	await expect(page.getByTestId('amount')).toContainText('0.700');
	// Tab to the scenario group, arrow to a decline and back to "Approve", Tab to Pay.
	await tabTo(page, 'input[type="radio"]');
	expect(await focusVisible(page)).toBe(true);
	await page.keyboard.press('ArrowDown');
	await expect(radio(page, /insufficient funds/)).toBeChecked();
	await page.keyboard.press('ArrowUp');
	await expect(radio(page, /^Approve/)).toBeChecked();
	await tabTo(page, 'button[type="submit"]');
	expect(await focusVisible(page)).toBe(true);
	await page.keyboard.press('Enter');
	await expect(statusOf(page)).toHaveAttribute('data-state', 'paid');
});

test('there is no card field anywhere in the checkout', async ({ page, request }) => {
	const shop = await project(request);
	const session = await checkoutSession(request, shop.admin, { amount: 100, currency: 'USD' });
	for (const url of [session.url, env.checkout, `${env.checkout}/c/not-a-session`]) {
		await page.goto(url);
		await page.waitForLoadState('networkidle');
		const fields = await page.evaluate(() =>
			[...document.querySelectorAll('input, textarea, select, [contenteditable]')].map((el) => ({
				type: el.getAttribute('type'),
				name: el.getAttribute('name'),
				autocomplete: el.getAttribute('autocomplete'),
				inputmode: el.getAttribute('inputmode')
			}))
		);
		for (const field of fields) {
			expect(field.type).toBe('radio');
			expect(field.name).toBe('scenario');
			expect(field.autocomplete ?? '').not.toMatch(/^cc-/);
			expect(field.inputmode).toBeNull();
		}
		await expect(page.locator('body')).not.toContainText(/card number|cvc|cvv|expiry/i);
	}
});

test('the checkout is framed only by the origins its session names, and tells only them the outcome', async ({
	page,
	request
}) => {
	const shop = await project(request);
	const embeddable = await checkoutSession(request, shop.admin, {
		amount: 300,
		currency: 'USD',
		embed_origins: [env.merchant]
	});
	const plain = await checkoutSession(request, shop.admin, { amount: 300, currency: 'USD' });
	const consoleLines: string[] = [];
	page.on('console', (message) => consoleLines.push(message.text()));
	// A merchant page on another loopback origin (e2e/merchant.mjs) frames both sessions.
	const query = new URLSearchParams({ allowed: embeddable.url, refused: plain.url });
	await page.goto(`${env.merchant}/shop?${query}`);
	const allowed = page.frameLocator('#allowed');
	await expect(allowed.getByTestId('amount')).toContainText('3.00');
	await allowed.getByRole('radio', { name: /^Approve/ }).check();
	await allowed.getByRole('button', { name: /^Pay / }).click();
	await expect(allowed.getByTestId('status')).toHaveAttribute('data-state', 'paid');
	await expect
		.poll(() => page.evaluate(() => (window as unknown as { messages: { data: { state: string } }[] }).messages.map((m) => m.data.state)))
		.toContain('paid');
	const messages = await page.evaluate(() => (window as unknown as { messages: { origin: string }[] }).messages);
	for (const message of messages) expect(message.origin).toBe(env.checkout);
	// The session that names no origin refuses to be framed: the browser blocks it by its
	// policy, and the frame never renders the page.
	await expect.poll(() => consoleLines.some((line) => line.includes("frame-ancestors 'none'"))).toBe(true);
	const refused = page.frameLocator('#refused');
	await expect(refused.getByTestId('amount')).toHaveCount(0);
});
