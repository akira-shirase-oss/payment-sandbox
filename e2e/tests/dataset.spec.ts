// The dataset reset in the dashboard (M7, D-022, D-066): an admin types the project's slug to
// open a new, empty epoch; nothing is deleted. Keyboard alone, every state, axe-clean.
// SIMULATED MONEY ONLY.
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { env, focusVisible, project, screens, seriousViolations, tabTo, watchPolicy } from './support.js';

async function pay(request: APIRequestContext, key: string, n: number) {
	const auth = await request.post(`${env.api}/v1/payments`, {
		headers: { authorization: `Bearer ${key}`, 'idempotency-key': `dataset-${n}` },
		data: { amount: 1_500, currency: 'USD', token: 'tok_sim_approve' }
	});
	expect(auth.status()).toBe(201);
	return (await auth.json()).payment.id as string;
}

async function signIn(page: Page, key: string) {
	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill(key);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
}

test('an admin resets the dataset by typing the slug, with the keyboard alone, and nothing is deleted', async ({ page, request }) => {
	const shop = await project(request, 'Reset shop');
	const old = await pay(request, shop.admin, 1);
	const violations = await watchPolicy(page);
	await signIn(page, shop.admin);
	await expect(page.getByTestId('header-epoch')).toHaveText('epoch 1');
	await page.getByRole('navigation', { name: 'Sections' }).getByRole('link', { name: 'Dataset' }).click();
	await expect(page.getByRole('heading', { name: 'Dataset', level: 1 })).toBeVisible();
	await expect(page.getByTestId('epoch')).toHaveText('1');
	const button = page.getByRole('button', { name: 'Reset and open epoch 2' });
	await expect(button).toBeDisabled();
	expect(await seriousViolations(page)).toEqual([]);
	await screens(page, 'dashboard-dataset');

	// A near miss keeps the button off and says so.
	await tabTo(page, '#confirm');
	expect(await focusVisible(page)).toBe(true);
	await page.keyboard.type(shop.slug.slice(0, -1));
	await expect(page.getByText('Not the slug yet.')).toBeVisible();
	await expect(button).toBeDisabled();
	await page.keyboard.type(shop.slug.slice(-1));
	await expect(button).toBeEnabled();
	await page.keyboard.press('Tab');
	expect(await page.evaluate(() => document.activeElement?.textContent?.trim())).toBe('Reset and open epoch 2');
	expect(await focusVisible(page)).toBe(true);
	await page.keyboard.press('Enter');

	await expect(page.getByTestId('reset-done')).toContainText('Epoch 2 is now current');
	await expect(page.getByTestId('reset-done')).toContainText('1 payment of epoch 1 stay archived');
	await expect(page.getByTestId('epoch')).toHaveText('2');
	await expect(page.getByTestId('header-epoch')).toHaveText('epoch 2');
	expect(await seriousViolations(page)).toEqual([]);
	await screens(page, 'dashboard-dataset-done');

	// The old payment is archived, not deleted: the API says so, and the new epoch is empty.
	const gone = await request.get(`${env.api}/v1/payments/${old}`, { headers: { authorization: `Bearer ${shop.admin}` } });
	expect(gone.status()).toBe(410);
	expect((await gone.json()).error.code).toBe('dataset_archived');
	await page.getByRole('navigation', { name: 'Sections' }).getByRole('link', { name: 'Payments' }).click();
	await expect(page.getByTestId('payments-empty')).toContainText('No payments in this epoch yet');
	expect(await violations()).toEqual([]);
});

test('a reset someone else already made is reported, and a key without admin cannot reset', async ({ page, request }) => {
	const shop = await project(request, 'Raced shop');
	await signIn(page, shop.admin);
	await page.goto(`${env.dashboard}/dataset`);
	await expect(page.getByTestId('epoch')).toHaveText('1');
	// Another person resets first, through the API.
	const first = await request.post(`${env.api}/v1/project/reset`, {
		headers: { authorization: `Bearer ${shop.admin}` },
		data: { confirm: shop.slug, epoch: 1 }
	});
	expect(first.status()).toBe(200);
	await page.getByLabel(`Type ${shop.slug} to confirm`).fill(shop.slug);
	await page.getByRole('button', { name: 'Reset and open epoch 2' }).click();
	const refused = page.getByTestId('reset-refused');
	await expect(refused).toContainText('Someone reset this dataset already');
	await expect(page.getByTestId('epoch')).toHaveText('2');
	// The internal code is behind a details toggle.
	await expect(refused.getByText('epoch_changed', { exact: false })).toBeHidden();
	await refused.getByText('Details').click();
	await expect(refused.getByText('epoch_changed', { exact: false })).toBeVisible();
	expect(await seriousViolations(page)).toEqual([]);
	await screens(page, 'dashboard-dataset-refused');

	await page.getByRole('button', { name: 'Sign out' }).click();
	await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
	await signIn(page, shop.read);
	await page.goto(`${env.dashboard}/dataset`);
	await expect(page.getByTestId('needs-admin')).toBeVisible();
	await expect(page.getByLabel(`Type ${shop.slug} to confirm`)).toBeDisabled();
	expect(await seriousViolations(page)).toEqual([]);
});

test('a reset whose answer is lost is shown as unknown until it is read back, never as done', async ({ page, request }) => {
	const shop = await project(request, 'Lost answer shop');
	await signIn(page, shop.admin);
	await page.goto(`${env.dashboard}/dataset`);
	await expect(page.getByTestId('epoch')).toHaveText('1');
	// The server receives and applies the reset; its answer never reaches the page.
	await page.route('**/api/v1/project/reset', async (route) => {
		await route.fetch();
		await route.abort('connectionreset');
	});
	await page.getByLabel(`Type ${shop.slug} to confirm`).fill(shop.slug);
	await page.getByRole('button', { name: 'Reset and open epoch 2' }).click();
	const unknown = page.getByTestId('reset-unknown');
	await expect(unknown).toContainText('We could not tell whether the reset happened');
	await expect(page.getByTestId('reset-done')).toHaveCount(0);
	expect(await seriousViolations(page)).toEqual([]);
	await screens(page, 'dashboard-dataset-unknown');
	await unknown.getByRole('button', { name: 'Check what happened' }).click();
	await expect(page.getByTestId('reset-done')).toContainText('Epoch 2 is now current');
	await expect(page.getByTestId('epoch')).toHaveText('2');
});
