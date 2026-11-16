// M4 load scenario: webhooks under load. Purchases (authorize + capture) at a fixed rate
// with an endpoint registered at the development store; the deliverer signs and delivers
// every event while the store verifies, deduplicates, re-reads each payment and fulfills.
// The gate: every captured payment fulfilled exactly once, every event in the store's inbox,
// no delivery dead, and the ledger reconciled.
//
// MODE=full: 250 purchases/s for 2,000 s (1,000,000 API requests, 1,000,000 deliveries).
// MODE=smoke: 25 purchases/s for 20 s.
import http from 'k6/http';
import { check, fail, sleep } from 'k6';
import exec from 'k6/execution';
import { call, projects } from './lib.js';

const STORE = __ENV.STORE_URL || 'http://127.0.0.1:56092';
const full = (__ENV.MODE || 'smoke') === 'full';
const rate = full ? 250 : 25;
const seconds = full ? 2000 : 20;

export const options = {
  setupTimeout: '60s',
  teardownTimeout: '300s',
  scenarios: {
    purchases: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: `${seconds}s`,
      preAllocatedVUs: 100,
      maxVUs: 2000,
    },
  },
  thresholds: {
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:gate}': ['rate==1'],
    'http_req_duration{phase:load}': ['p(95)<100', 'p(99)<250'],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  const [key] = projects(1, 'webhooks');
  const reader = call('POST', '/v1/keys', key, { label: 'dev store', scopes: ['read'] });
  const endpoint = call('POST', '/v1/webhook_endpoints', key, { url: `${STORE}/webhooks` });
  if (reader.status !== 201 || endpoint.status !== 201) fail(`setup: ${reader.status} ${endpoint.status} ${endpoint.body}`);
  const config = http.put(`${STORE}/config`, JSON.stringify({
    sandbox_url: __ENV.BASE_URL || 'http://127.0.0.1:56090',
    api_key: reader.json().secret,
    secrets: [endpoint.json().secret],
  }), { headers: { 'content-type': 'application/json' } });
  if (config.status !== 200) fail(`store config: ${config.status}`);
  return { key };
}

export default function (data) {
  const n = exec.scenario.iterationInTest;
  const auth = call('POST', '/v1/payments', data.key, { amount: 1000, currency: 'USD', token: 'tok_sim_approve' }, `w-${n}`, { phase: 'load' });
  if (!check(auth, { 'authorized': (r) => r.status === 201 }, { phase: 'load' })) return;
  const cap = call('POST', `/v1/payments/${auth.json().payment.id}/captures`, data.key, { amount: 700 }, `wc-${n}`, { phase: 'load' });
  check(cap, { 'captured': (r) => r.status === 201 }, { phase: 'load' });
}

export function teardown(data) {
  const report = call('GET', '/v1/reconciliation', data.key, undefined, null, { phase: 'gate' }).json();
  const paid = report.report.payments_checked;
  let stats = null;
  for (let i = 0; i < 280; i++) {
    stats = http.get(`${STORE}/stats`).json();
    if (stats.fulfillments >= paid && stats.inbox >= 2 * paid) break;
    sleep(1);
  }
  console.log(`webhooks: payments=${paid} store=${JSON.stringify(stats)} reconciliation=${JSON.stringify(report.report)}`);
  check(stats, {
    'every captured payment fulfilled exactly once': (s) => s.fulfillments === paid,
    'every event reached the store inbox': (s) => s.inbox === 2 * paid,
    'no forged or rejected delivery': (s) => s.rejected === 0,
  }, { phase: 'gate' });
  check(report, { 'ledger reconciled': (r) => r.clean === true }, { phase: 'gate' });
}
