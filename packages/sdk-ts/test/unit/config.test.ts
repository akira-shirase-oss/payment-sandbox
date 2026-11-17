// Production-looking configuration is refused before anything is sent (D-068).
import { describe, expect, it } from 'vitest';
import { ConfigError, Sandbox, checkConfig } from '../../src/index.ts';

const KEY = `sk_sim_${'0123456789abcdef'.repeat(4)}`;
const env = {};

describe('the configuration', () => {
	it('accepts loopback URLs and sandbox keys', () => {
		for (const apiUrl of [
			'http://127.0.0.1:56080',
			'http://127.0.0.1:56080/',
			'http://localhost:56080',
			'http://dashboard.localhost:56084',
			'http://[::1]:56080',
			'https://localhost'
		]) {
			expect(checkConfig({ apiKey: KEY, apiUrl }, env).apiUrl.endsWith('/')).toBe(false);
		}
	});

	it('refuses a real processor, a remote host without the override, and any key that is not a sandbox key', () => {
		const refusal = (fn: () => unknown) => {
			try {
				fn();
			} catch (error) {
				expect(error).toBeInstanceOf(ConfigError);
				return (error as ConfigError).code;
			}
			throw new Error('accepted');
		};
		expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl: 'https://api.stripe.com', allowNonLocal: true }, env))).toBe('real_processor');
		expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl: 'https://checkout-test.adyen.com', allowNonLocal: true }, env))).toBe(
			'real_processor'
		);
		expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl: 'https://sandbox.example.com' }, env))).toBe('non_local');
		expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl: 'http://10.0.0.5:56080' }, env))).toBe('non_local');
		// v1 review L3: off this machine, plain http would carry the key in clear text: https,
		// or an explicit second opt-in for a private network (a CI service container).
		expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl: 'http://sandbox:56080', allowNonLocal: true }, env))).toBe('plain_http');
		expect(checkConfig({ apiKey: KEY, apiUrl: 'https://sandbox.example.com', allowNonLocal: true }, env).apiUrl).toBe(
			'https://sandbox.example.com'
		);
		expect(
			checkConfig({ apiKey: KEY, apiUrl: 'http://sandbox:56080', allowNonLocal: true, allowPlainHttp: true }, env).apiUrl
		).toBe('http://sandbox:56080');
		// Built at run time, so no literal in the source looks like a real processor's key.
		const hex = '0123456789abcdef'.repeat(4);
		for (const apiKey of [
			['sk', 'live', hex].join('_'),
			['sk', 'test', hex].join('_'),
			['rk', 'live', 'abc'].join('_'),
			'sk_sim_short',
			`sk_sim_${'0123456789ABCDEF'.repeat(4)}`
		]) {
			try {
				checkConfig({ apiKey }, env);
				throw new Error(`accepted ${apiKey}`);
			} catch (error) {
				expect((error as ConfigError).code).toBe('not_a_sandbox_key');
				expect((error as Error).message).not.toContain(apiKey);
			}
		}
		expect(refusal(() => checkConfig({ apiKey: '  ' }, env))).toBe('missing_key');
		for (const apiUrl of ['127.0.0.1:56080', 'ftp://127.0.0.1', 'http://user:pw@127.0.0.1:56080', 'http://127.0.0.1:56080/v1', 'http://127.0.0.1:56080?x=1']) {
			expect(refusal(() => checkConfig({ apiKey: KEY, apiUrl }, env))).toBe('bad_url');
		}
	});

	it('refuses NODE_ENV=production unless a test of a production build says so', () => {
		expect(() => checkConfig({ apiKey: KEY }, { NODE_ENV: 'production' })).toThrow(/NODE_ENV is production/);
		expect(checkConfig({ apiKey: KEY, allowProductionEnvironment: true }, { NODE_ENV: 'production' }).apiKey).toBe(KEY);
	});

	it('refuses before anything is sent', () => {
		let calls = 0;
		const fetch = (async () => {
			calls += 1;
			return new Response('{}');
		}) as typeof globalThis.fetch;
		expect(() => new Sandbox({ apiKey: KEY, apiUrl: 'https://api.stripe.com', allowNonLocal: true, fetch })).toThrow(ConfigError);
		expect(calls).toBe(0);
	});
});
