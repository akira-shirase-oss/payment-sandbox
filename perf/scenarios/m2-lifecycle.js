// M2 load scenario: the guide's workload — two projects, the lifecycle (authorize, capture
// 600 of 1,000, refund 200) at a sustained rate with 5× short bursts — then the correctness
// gate: zero imbalance and every payment equal to its journal entries.
//
// MODE=full: 100 purchases/s (300 operations/s) with a 5× burst for 10 s in every 100 s,
// 3,000 s: about 1,170,000 requests. MODE=smoke: 25/s with one 5× burst, 24 s.
import { check } from 'k6';
import exec from 'k6/execution';
import { call, projects, reconcile, statusThresholds } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
// RATE/ROUNDS override the full shape for a shorter reproduction (CI leaves them unset).
const base = full ? Number(__ENV.RATE || 100) : 25;
const rounds = Number(__ENV.ROUNDS || 30);

function stages() {
  if (!full) {
    return [
      { target: base, duration: '1s' }, { target: base, duration: '10s' },
      { target: base * 5, duration: '1s' }, { target: base * 5, duration: '4s' },
      { target: base, duration: '1s' }, { target: base, duration: '7s' },
    ];
  }
  const out = [{ target: base, duration: '5s' }];
  for (let i = 0; i < rounds; i++) {
    out.push({ target: base, duration: '88s' }, { target: base * 5, duration: '1s' },
      { target: base * 5, duration: '10s' }, { target: base, duration: '1s' });
  }
  return out;
}

export const options = {
  setupTimeout: '120s',
  teardownTimeout: '120s',
  scenarios: {
    purchases: {
      executor: 'ramping-arrival-rate',
      startRate: base,
      timeUnit: '1s',
      stages: stages(),
      preAllocatedVUs: 100,
      maxVUs: 2000,
    },
  },
  thresholds: {
    'http_req_failed{phase:load}': ['rate<0.0001'],
    'http_req_duration{op:authorize}': ['p(95)<100', 'p(99)<250'],
    'http_req_duration{op:capture}': ['p(95)<100', 'p(99)<250'],
    'http_req_duration{op:refund}': ['p(95)<100', 'p(99)<250'],
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:reconcile}': ['rate==1'],
    ...statusThresholds(['authorize', 'capture', 'refund']),
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  return { keys: projects(2, 'lifecycle') };
}

export default function (data) {
  const key = data.keys[exec.scenario.iterationInTest % 2];
  // Letters between the parts: the server refuses an idempotency key whose digits, joined
  // across separators, could be a card number (IdempotencyKey::parse, card_guard).
  const id = `vu${__VU}-it${__ITER}-${Date.now().toString(36)}`;
  const tags = (op) => ({ op, phase: 'load' });
  const auth = call('POST', '/v1/payments', key, { amount: 1000, currency: 'USD', token: 'tok_sim_approve' }, `a-${id}`, tags('authorize'));
  if (!check(auth, { 'authorized': (r) => r.status === 201 }, { phase: 'load' })) return;
  const pid = auth.json().payment.id;
  const cap = call('POST', `/v1/payments/${pid}/captures`, key, { amount: 600 }, `c-${id}`, tags('capture'));
  check(cap, { 'captured': (r) => r.status === 201 }, { phase: 'load' });
  const ref = call('POST', `/v1/payments/${pid}/refunds`, key, { amount: 200 }, `r-${id}`, tags('refund'));
  check(ref, { 'refunded': (r) => r.status === 201 }, { phase: 'load' });
}

export function teardown(data) {
  reconcile(data.keys);
}
