// What every journey needs: the addresses scripts/e2e.sh passed, projects made through the
// operator's bootstrap token, sessions made as the merchant would, and a watch for policy
// violations. SIMULATED MONEY ONLY: every amount, key and token here is synthetic.
import { expect, type APIRequestContext, type Page } from '@playwright/test';

function required(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is not set: run the journeys with scripts/e2e.sh`);
	return value;
}

export const env = {
	api: required('E2E_API'),
	checkout: required('E2E_CHECKOUT'),
	dashboard: required('E2E_DASHBOARD'),
	store: required('E2E_STORE'),
	merchant: required('E2E_MERCHANT'),
	bootstrap: required('E2E_BOOTSTRAP'),
	storeToken: required('E2E_STORE_TOKEN'),
	simControl: required('E2E_SIM_CONTROL')
};

let counter = 0;

/** A fresh project; its admin key and a read-only key. */
export async function project(request: APIRequestContext, name = 'E2E shop') {
	counter += 1;
	const slug = `e2e-${Date.now().toString(36)}-${counter}`;
	const created = await request.post(`${env.api}/v1/projects`, {
		headers: { authorization: `Bearer ${env.bootstrap}` },
		data: { slug, name }
	});
	expect(created.status(), await created.text()).toBe(201);
	const body = await created.json();
	const admin: string = body.api_key.secret;
	const reader = await request.post(`${env.api}/v1/keys`, {
		headers: { authorization: `Bearer ${admin}` },
		data: { label: 'e2e reader', scopes: ['read'] }
	});
	expect(reader.status()).toBe(201);
	return { id: body.project.id as string, slug, admin, read: (await reader.json()).secret as string };
}

/** A checkout session, created as the merchant's server would. */
export async function checkoutSession(request: APIRequestContext, key: string, data: Record<string, unknown>) {
	const created = await request.post(`${env.api}/v1/checkout_sessions`, {
		headers: { authorization: `Bearer ${key}` },
		data
	});
	expect(created.status(), await created.text()).toBe(201);
	const body = await created.json();
	return { id: body.session.id as string, secret: body.client_secret as string, url: body.url as string };
}

/** The merchant's view of a session: its attempts and their payments. */
export async function merchantView(request: APIRequestContext, key: string, id: string) {
	const answer = await request.get(`${env.api}/v1/checkout_sessions/${id}`, {
		headers: { authorization: `Bearer ${key}` }
	});
	expect(answer.status()).toBe(200);
	return answer.json();
}

export async function api(request: APIRequestContext, key: string, path: string) {
	const answer = await request.get(`${env.api}${path}`, { headers: { authorization: `Bearer ${key}` } });
	expect(answer.status(), await answer.text()).toBe(200);
	return answer.json();
}

/** Collects every Content-Security-Policy violation the page reports, from the first byte. */
export async function watchPolicy(page: Page): Promise<() => Promise<string[]>> {
	const seen: string[] = [];
	page.on('console', (message) => {
		if (/Content Security Policy|Refused to/i.test(message.text())) seen.push(`console: ${message.text()}`);
	});
	await page.addInitScript(() => {
		const store = window as unknown as { __violations?: string[] };
		store.__violations = [];
		document.addEventListener('securitypolicyviolation', (event) =>
			store.__violations?.push(`${event.violatedDirective} ${event.blockedURI}`)
		);
	});
	return async () => {
		const inPage = await page
			.evaluate(() => (window as unknown as { __violations?: string[] }).__violations ?? [])
			.catch(() => []);
		return [...seen, ...inPage];
	};
}

/** The payer-facing state the page shows (the status region's data-state). */
export const statusOf = (page: Page) => page.getByTestId('status');

/** Presses Tab until the focused element matches, as a keyboard user would. */
export async function tabTo(page: Page, matches: string, limit = 25) {
	for (let i = 0; i < limit; i += 1) {
		await page.keyboard.press('Tab');
		if (await page.evaluate((selector) => document.activeElement?.matches(selector) ?? false, matches)) return;
	}
	throw new Error(`Tab never reached ${matches}`);
}

/** True when the focused element shows a focus indicator (an outline or a ring). */
export async function focusVisible(page: Page): Promise<boolean> {
	return page.evaluate(() => {
		const el = document.activeElement;
		if (!el) return false;
		const style = getComputedStyle(el);
		return style.boxShadow !== 'none' || (style.outlineStyle !== 'none' && style.outlineWidth !== '0px');
	});
}

/** No horizontal scroll on the page. */
export async function noSidewaysScroll(page: Page) {
	const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
	expect(overflow).toBeLessThanOrEqual(0);
}

/** Every outcome badge shows its whole word and sits inside the viewport. */
export async function badgesWhole(page: Page) {
	const clipped = await page.evaluate(() =>
		[...document.querySelectorAll<HTMLElement>('[data-slot="outcome"]')]
			.filter((badge) => badge.offsetParent !== null)
			.filter((badge) => {
				const box = badge.getBoundingClientRect();
				return badge.scrollWidth > badge.clientWidth + 1 || box.right > window.innerWidth + 0.5 || box.left < -0.5;
			})
			.map((badge) => badge.textContent?.trim())
	);
	expect(clipped).toEqual([]);
}
