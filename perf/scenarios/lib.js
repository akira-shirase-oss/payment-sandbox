// Shared helpers for the payment scenarios. SIMULATED MONEY ONLY: every request goes to
// the local server, whose network is the local simulator.
import http from 'k6/http';
import { check, fail } from 'k6';

export const BASE = __ENV.BASE_URL || 'http://127.0.0.1:56090';

export function call(method, path, key, body, idem, tags) {
  const headers = { authorization: `Bearer ${key}` };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (idem) headers['idempotency-key'] = idem;
  // The URL's ids are folded into the `name` tag (`/v1/payments/{id}/captures`): k6 keeps one
  // time series per distinct tag set, and a tag per payment grew to 6.4 million series in
  // the first full m3 run, costing the load generator the memory and CPU it shares with
  // the server (D-090).
  const name = `${BASE}${path.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, '{id}').split('?')[0]}`;
  return http.request(method, `${BASE}${path}`, body === undefined ? null : JSON.stringify(body), {
    headers,
    responseType: 'text',
    tags: { name, ...tags },
  });
}

// Creates `n` projects with the operator token; returns their admin keys.
export function projects(n, prefix) {
  // Base 36, not decimal: a 13-digit millisecond stamp next to the index digit can pass
  // Luhn, and the server then refuses the slug as card data (card_guard, D-059).
  const run = Date.now().toString(36);
  const keys = [];
  for (let i = 0; i < n; i++) {
    const res = call('POST', '/v1/projects', __ENV.BOOTSTRAP, { slug: `${prefix}-${i}-${run}`, name: `${prefix} ${i}` });
    if (res.status !== 201) fail(`project: ${res.status} ${res.body}`);
    keys.push(res.json().api_key.secret);
  }
  return keys;
}

// The correctness gate: zero imbalance, every payment equal to its journal, nothing open.
export function reconcile(keys) {
  for (const key of keys) {
    const res = call('GET', '/v1/reconciliation', key, undefined, null, { phase: 'reconcile' });
    const body = res.status === 200 ? res.json() : null;
    check(body, {
      'reconciliation clean': (b) => b !== null && b.clean === true,
      'no operation left open': (b) => b !== null && b.report.open_operations === 0,
    }, { phase: 'reconcile' });
    if (body) console.log(`reconciliation: ${JSON.stringify(body.report)}`);
  }
}
