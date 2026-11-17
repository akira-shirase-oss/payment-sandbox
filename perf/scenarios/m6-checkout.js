// M6 load scenario: the hosted checkout as payers use it. Each iteration is one checkout:
// the merchant creates a session (API), the payer's browser loads the page (checkout
// origin, served from memory), reads the session, pays with "approve" (an authorization
// and a capture at the network) and, as a double click would, sends the same attempt again
// under the same key. Then the correctness gate: every session paid exactly once, zero
// imbalance, nothing open.
//
// MODE=full: 100 checkouts/s for 2,000 s, 5 requests each: about 1,000,000 requests.
// MODE=smoke: 20 checkouts/s for 20 s. Needs CHECKOUT_URL (scripts/load.sh starts the
// checkout listener for m6 scenarios).
import { check, fail } from 'k6';
import exec from 'k6/execution';
import http from 'k6/http';
import { call, projects, reconcile } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
const rate = full ? 100 : 20;
const CHECKOUT = __ENV.CHECKOUT_URL || fail('CHECKOUT_URL is required (scripts/load.sh sets it)');

export const options = {
  setupTimeout: '120s',
  teardownTimeout: '300s',
  scenarios: {
    checkouts: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: full ? '2000s' : '20s',
      preAllocatedVUs: 100,
      maxVUs: 2000,
    },
  },
  thresholds: {
    'http_req_failed{phase:load}': ['rate<0.0001'],
    'http_req_duration{op:session}': ['p(95)<100', 'p(99)<250'],
    'http_req_duration{op:page}': ['p(95)<20', 'p(99)<50'],
    'http_req_duration{op:view}': ['p(95)<50', 'p(99)<100'],
    // Authorize and capture, two network calls in one request.
    'http_req_duration{op:pay}': ['p(95)<200', 'p(99)<400'],
    'http_req_duration{op:repeat}': ['p(95)<100', 'p(99)<250'],
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:reconcile}': ['rate==1'],
    'checks{phase:gate}': ['rate==1'],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  return { keys: projects(2, 'checkout') };
}

function payer(method, path, secret, body, idem, op) {
  const headers = { authorization: `Bearer ${secret}`, origin: CHECKOUT };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (idem) headers['idempotency-key'] = idem;
  return http.request(method, `${CHECKOUT}${path}`, body === undefined ? null : JSON.stringify(body), {
    headers,
    responseType: 'text',
    tags: { op, phase: 'load' },
  });
}

export default function (data) {
  const key = data.keys[exec.scenario.iterationInTest % 2];
  const created = call('POST', '/v1/checkout_sessions', key, { amount: 1250, currency: 'USD', description: 'load order' },
    null, { op: 'session', phase: 'load' });
  if (!check(created, { 'session created': (r) => r.status === 201 }, { phase: 'load' })) return;
  const { session, client_secret: secret } = created.json();
  const page = http.get(`${CHECKOUT}/c/${session.id}`, { tags: { op: 'page', phase: 'load' }, responseType: 'none' });
  check(page, { 'page served under its policy': (r) => r.status === 200 && (r.headers['Content-Security-Policy'] || '').includes("frame-ancestors 'none'") }, { phase: 'load' });
  const view = payer('GET', `/api/sessions/${session.id}`, secret, undefined, null, 'view');
  check(view, { 'session open': (r) => r.status === 200 && r.json().session.state === 'open' }, { phase: 'load' });
  const idem = `${exec.vu.idInTest}-${exec.vu.iterationInScenario}-${Date.now()}-a`;
  const paid = payer('POST', `/api/sessions/${session.id}/attempts`, secret, { token: 'tok_sim_approve' }, idem, 'pay');
  check(paid, { 'paid': (r) => r.status === 200 && r.json().session.state === 'paid' }, { phase: 'load' });
  const again = payer('POST', `/api/sessions/${session.id}/attempts`, secret, { token: 'tok_sim_approve' }, idem, 'repeat');
  check(again, { 'a repeat is the same attempt': (r) => r.status === 200 && r.json().attempt.number === 1 && r.json().session.state === 'paid' }, { phase: 'load' });
}

export function teardown(data) {
  reconcile(data.keys);
  // Every payment of the run is captured once: as many captured payments as paid sessions,
  // and no session with a second attempt (sampled through the payment list's first page).
  for (const key of data.keys) {
    const list = call('GET', '/v1/payments?limit=500', key, undefined, null, { phase: 'gate' });
    const payments = list.status === 200 ? list.json().payments : [];
    check(payments, {
      'every listed payment captured exactly its amount': (ps) => ps.length > 0 && ps.every((p) => p.status === 'captured' && p.captured === 1250),
    }, { phase: 'gate' });
  }
}
