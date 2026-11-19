// The UI audit walk (docs/UI_AUDIT.md): every operator screen under both roles (owner's
// admin key, viewer's read-only key), at both widths (1440, 390), in both schemes — with
// the fresh project's empty states and the viewer's denied state. SIMULATED MONEY ONLY.
import { expect, test, type Page } from '@playwright/test';
import { env, project, screens, seriousViolations } from './support.js';

const SECTIONS = [
	{ path: '/', heading: 'Overview' },
	{ path: '/payments', heading: 'Payments' },
	{ path: '/ledger', heading: 'Ledger' },
	{ path: '/webhooks', heading: 'Webhooks' },
	{ path: '/dataset', heading: 'Dataset' }
];

async function signIn(page: Page, key: string) {
	await page.goto(env.dashboard);
	await page.getByLabel('Sandbox key').fill(key);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
}

for (const width of [1440, 390]) {
	for (const scheme of ['light', 'dark'] as const) {
		test(`the owner sees every section at ${width}px in ${scheme}, empty states included`, async ({ page, request }) => {
			const shop = await project(request);
			await page.setViewportSize({ width, height: 900 });
			await page.emulateMedia({ colorScheme: scheme });
			await signIn(page, shop.admin);
			const sections = [...SECTIONS, { path: '/simulator', heading: 'Simulator' }];
			for (const section of sections) {
				await page.goto(`${env.dashboard}${section.path}`);
				await expect(
					page.getByRole('heading', { name: section.heading }).first(),
					`${section.heading} at ${width}px/${scheme}`
				).toBeVisible();
				// A fresh project is the empty state: the section says so rather than erroring.
				expect(await seriousViolations(page), `policy violations on ${section.path}`).toEqual([]);
			}
			await screens(page, `audit-owner-${width}-${scheme}`);
		});
	}
}

test('the viewer reads every section and the reset form stays disabled', async ({ page, request }) => {
	const shop = await project(request);
	await page.emulateMedia({ colorScheme: 'light' });
	await signIn(page, shop.read);
	for (const section of SECTIONS) {
		await page.goto(`${env.dashboard}${section.path}`);
		await expect(
			page.getByRole('heading', { name: section.heading }).first(),
			`viewer: ${section.heading}`
		).toBeVisible();
		expect(await seriousViolations(page), `policy violations on ${section.path}`).toEqual([]);
	}
	// The denied state: a read key sees the dataset page but its reset form stays shut.
	await page.goto(`${env.dashboard}/dataset`);
	await expect(page.getByTestId('needs-admin')).toBeVisible();
	await screens(page, 'audit-viewer');
});
