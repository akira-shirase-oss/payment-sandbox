// Where the SDK talks to and with which key, checked before anything is sent (D-068).
// SIMULATED MONEY ONLY: a production-looking configuration is refused unless the caller
// explicitly overrides the one rule that can have a legitimate exception (a sandbox on
// another machine); a real processor's key or host is never accepted.

import { ConfigError } from './errors.ts';

/** The sandbox API's default address. */
export const DEFAULT_API_URL = 'http://127.0.0.1:56080';

/** Hosts of real payment processors (and their subdomains): never a sandbox. */
export const REAL_PROCESSOR_HOSTS: readonly string[] = [
	'stripe.com',
	'stripe.network',
	'adyen.com',
	'adyenpayments.com',
	'paypal.com',
	'braintreegateway.com',
	'squareup.com',
	'checkout.com',
	'authorize.net',
	'worldpay.com',
	'razorpay.com',
	'mollie.com',
	'klarna.com',
	'paddle.com'
];

const KEY_SHAPE = /^sk_sim_[0-9a-f]{64}$/;

export interface SandboxOptions {
	/** The project's sandbox key (`sk_sim_` + 64 hex characters). */
	apiKey: string;
	/** The sandbox API, `http://127.0.0.1:56080` by default. */
	apiUrl?: string;
	/**
	 * Allow an API that is not on this machine: a CI service container, or a shared sandbox
	 * you run. Never allows a real processor's host or key.
	 */
	allowNonLocal?: boolean;
	/**
	 * With `allowNonLocal`, also allow plain `http://` to that machine, which sends your key in
	 * clear text: only on a private network you control (a CI service container).
	 */
	allowPlainHttp?: boolean;
	/**
	 * Allow use while `NODE_ENV` is `production`. A production build pointed at the sandbox
	 * is usually a mistake (no order may ever be fulfilled from simulated money), so this is
	 * refused unless you say it is a test of a production build.
	 */
	allowProductionEnvironment?: boolean;
	/** Retry and deadline settings. */
	retry?: Partial<RetryPolicy>;
	/** A `fetch` to use instead of the global one (tests). */
	fetch?: typeof fetch;
}

export interface RetryPolicy {
	/** Attempts in all, the first included. */
	maxAttempts: number;
	/** The first backoff's ceiling, in ms; each later one doubles, up to `capMs`. */
	baseMs: number;
	/** The largest backoff ceiling, in ms. */
	capMs: number;
	/** Deadline of one attempt, in ms. */
	attemptTimeoutMs: number;
	/** Deadline of a request, retries included, in ms. */
	deadlineMs: number;
}

export const DEFAULT_RETRY: RetryPolicy = {
	maxAttempts: 4,
	baseMs: 100,
	capMs: 2_000,
	attemptTimeoutMs: 10_000,
	deadlineMs: 30_000
};

/** A checked configuration. */
export interface CheckedConfig {
	apiUrl: string;
	apiKey: string;
	retry: RetryPolicy;
}

/** The lowercased host of an `http(s)://host[:port][/]` URL, or a ConfigError. */
export function urlHost(url: string): string {
	const bad = () =>
		new ConfigError(
			'bad_url',
			'The API URL must look like http://127.0.0.1:56080 (http or https, a host, an optional port, no user, path, query or fragment).'
		);
	let parsed: URL;
	try {
		parsed = new URL(url);
	} catch {
		throw bad();
	}
	if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') throw bad();
	if (parsed.username || parsed.password || parsed.search || parsed.hash) throw bad();
	if (parsed.pathname !== '/' || /[?#@]/.test(url.slice(url.indexOf('//') + 2))) throw bad();
	if (url.replace(/\/$/, '').split('//')[1]?.includes('/')) throw bad();
	if (parsed.port === '0') throw bad();
	const host = parsed.hostname.toLowerCase();
	if (host === '') throw bad();
	return host.startsWith('[') && host.endsWith(']') ? host.slice(1, -1) : host;
}

/** `localhost`, `*.localhost`, 127.0.0.0/8 and `::1`. */
export function isLoopbackHost(host: string): boolean {
	const h = host.replace(/\.$/, '');
	if (h === 'localhost' || h.endsWith('.localhost')) return true;
	if (h === '::1') return true;
	const parts = h.split('.');
	return parts.length === 4 && parts[0] === '127' && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255);
}

/** A real processor's host or one of its subdomains. */
export function isRealProcessor(host: string): boolean {
	const h = host.replace(/\.$/, '');
	return REAL_PROCESSOR_HOSTS.some((real) => h === real || h.endsWith(`.${real}`));
}

/** Checks `options` (D-068); throws a ConfigError that says what to do. The key is never echoed. */
export function checkConfig(options: SandboxOptions, env: Record<string, string | undefined> = processEnv()): CheckedConfig {
	const apiUrl = options.apiUrl ?? DEFAULT_API_URL;
	const host = urlHost(apiUrl);
	if (isRealProcessor(host)) {
		throw new ConfigError(
			'real_processor',
			`${host} belongs to a real payment processor; Payment Sandbox never talks to one (SIMULATED MONEY ONLY).`
		);
	}
	if (!options.allowNonLocal && !isLoopbackHost(host)) {
		throw new ConfigError(
			'non_local',
			`${host} is not this machine: the sandbox runs on loopback (127.0.0.1, ::1, localhost). If it runs in a CI service container or on a shared host you control, pass allowNonLocal: true.`
		);
	}
	if (!isLoopbackHost(host) && !options.allowPlainHttp && new URL(apiUrl).protocol === 'http:') {
		throw new ConfigError(
			'plain_http',
			`${host} is not this machine, and http:// would send your sandbox key in clear text: use its https:// URL. Only on a private network you control (a CI service container) pass allowPlainHttp: true as well.`
		);
	}
	const key = (options.apiKey ?? '').trim();
	if (key === '') {
		throw new ConfigError('missing_key', 'No sandbox key: pass apiKey (sandbox keys start with sk_sim_; create one with POST /v1/keys).');
	}
	if (!KEY_SHAPE.test(key)) {
		throw new ConfigError(
			'not_a_sandbox_key',
			"That is not a Payment Sandbox key: sandbox keys are sk_sim_ followed by 64 hex characters. Never use a real processor's key (sk_live_, sk_test_, rk_...) with the sandbox."
		);
	}
	if (env.NODE_ENV === 'production' && !options.allowProductionEnvironment) {
		throw new ConfigError(
			'production_environment',
			'NODE_ENV is production: a production build must never take payments from the sandbox. If this is a test of a production build, pass allowProductionEnvironment: true.'
		);
	}
	const retry = { ...DEFAULT_RETRY, ...options.retry };
	if (!(retry.maxAttempts >= 1 && retry.maxAttempts <= 10) || !(retry.deadlineMs > 0 && retry.deadlineMs <= 600_000)) {
		throw new ConfigError('bad_retry', 'retry.maxAttempts must be 1-10 and retry.deadlineMs 1-600000.');
	}
	return { apiUrl: apiUrl.replace(/\/$/, ''), apiKey: key, retry };
}

function processEnv(): Record<string, string | undefined> {
	return typeof process === 'undefined' ? {} : process.env;
}
