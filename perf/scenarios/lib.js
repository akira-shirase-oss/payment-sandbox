// Shared helpers for the payment scenarios. SIMULATED MONEY ONLY: every request goes to
// the local server, whose network is the local simulator.
import http from 'k6/http';
import { check, fail, sleep } from 'k6';
import { Counter } from 'k6/metrics';

export const BASE = __ENV.BASE_URL || 'http://127.0.0.1:56090';

// Status codes per op (m2/m3's 2026-11-19 full misses: the failed rate alone could not say
// which answer the failures were — timeouts, sheds or honest non-2xx states).
const statuses = new Counter('op_status');

export function call(method, path, key, body, idem, tags) {
  const headers = { authorization: `Bearer ${key}` };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (idem) headers['idempotency-key'] = idem;
  // The URL's ids are folded into the `name` tag (`/v1/payments/{id}/captures`): k6 keeps one
  // time series per distinct tag set, and a tag per payment grew to 6.4 million series in
  // the first full m3 run, costing the load generator the memory and CPU it shares with
  // the server (D-090).
  const name = `${BASE}${path.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, '{id}').split('?')[0]}`;
  const res = http.request(method, `${BASE}${path}`, body === undefined ? null : JSON.stringify(body), {
    headers,
    responseType: 'text',
    tags: { name, ...tags },
  });
  // `0` is what k6 reports for a request that never got a response (its own timeout).
  statuses.add(1, { op: (tags && (tags.op || tags.path)) || 'other', status: String(res.status) });
  return res;
}

// The JSON summary keeps only threshold-referenced sub-metrics: enumerate the plausible
// answers per op so every op's status counts survive `--summary-export` (D-097).
export function statusThresholds(ops) {
  const t = {};
  for (const op of [...ops, 'other']) {
    for (const s of ['0', '200', '201', '202', '400', '401', '403', '404', '409', '422', '429', '500', '502', '503', '504']) {
      // `count>-1`, not `count>=0`: k6's threshold grammar does not accept `>=` — the
      // sub-metrics exported fine but every threshold showed as breached (m2 run 44535136879).
      t[`op_status{op:${op},status:${s}}`] = ['count>-1'];
    }
  }
  return t;
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
// Operations legitimately close AFTER the load stops — the sweeps only see them past a
// grace (`reconcile_grace`) and their `next_check_at` backoff — so the gate waits a bounded
// drain window instead of demanding zero the instant the offer ends (D-097).
const drain = Number(__ENV.RECONCILE_DRAIN_MS || 60000);

export function reconcile(keys) {
  for (const key of keys) {
    let body = null;
    const deadline = Date.now() + drain;
    for (;;) {
      const res = call('GET', '/v1/reconciliation', key, undefined, null, { phase: 'reconcile' });
      body = res.status === 200 ? res.json() : null;
      if (body === null || body.report.open_operations === 0 || Date.now() >= deadline) break;
      sleep(2);
    }
    check(body, {
      'reconciliation clean': (b) => b !== null && b.clean === true,
      'no operation left open': (b) => b !== null && b.report.open_operations === 0,
    }, { phase: 'reconcile' });
    if (body) console.log(`reconciliation: ${JSON.stringify(body.report)}`);
  }
}
