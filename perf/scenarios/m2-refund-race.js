// M2 load scenario: the hot-account and hot-payment refund races (D-027). One merchant
// account with HOT_ACCOUNT payments of 300 captured, plus one hot payment of 300, receive
// partial refunds of 10 at a fixed rate, more than their capacity. Exactly 30 refunds per
// payment may succeed; every other one must fail with 409 insufficient_capacity; then the
// reconciliation must be clean.
//
// MODE=full: 400 payments, 600 refunds/s for 60 s (36,000 offered, 12,030 admitted).
// MODE=smoke: 20 payments, 200 refunds/s for 10 s (2,000 offered, 630 admitted).
import { check } from 'k6';
import exec from 'k6/execution';
import { Counter } from 'k6/metrics';
import { call, projects, reconcile, seedSettled } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
const accountPayments = full ? 400 : 20;
const rate = full ? 600 : 200;
const seconds = full ? 60 : 10;
const admittedTarget = (accountPayments + 1) * 30;

const admitted = new Counter('refunds_admitted');
const refused = new Counter('refunds_refused');
const shed = new Counter('refunds_shed');

export const options = {
  setupTimeout: '300s',
  teardownTimeout: '300s',
  scenarios: {
    race: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: `${seconds}s`,
      preAllocatedVUs: 200,
      maxVUs: 1000,
    },
  },
  thresholds: {
    'refunds_admitted': [`count==${admittedTarget}`],
    'checks{phase:race}': ['rate==1'],
    'checks{phase:reconcile}': ['rate==1'],
    'http_req_duration{op:refund}': ['p(99)<500'],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

function captured(key, n) {
  const auth = call('POST', '/v1/payments', key, { amount: 300, currency: 'EUR', token: 'tok_sim_approve' }, `race-a-${n}`);
  seedSettled(auth, key, 'seed authorize');
  const pid = auth.json().payment.id;
  const cap = call('POST', `/v1/payments/${pid}/captures`, key, { amount: 300 }, `race-c-${n}`);
  seedSettled(cap, key, 'seed capture');
  return pid;
}

export function setup() {
  const [key] = projects(1, 'race');
  const payments = [];
  for (let n = 0; n < accountPayments; n++) payments.push(captured(key, n));
  const hot = captured(key, 'hot');
  return { key, payments, hot };
}

export default function (data) {
  // Half the refunds hammer one payment; half spread over the hot merchant account.
  // The test-wide iteration number (__ITER counts per virtual user, so it would only
  // ever reach the first few payments).
  const n = exec.scenario.iterationInTest;
  const pid = n % 2 === 0 ? data.hot : data.payments[(n >> 1) % data.payments.length];
  const res = call('POST', `/v1/payments/${pid}/refunds`, data.key, { amount: 10 }, `refund-${__VU}-${__ITER}`, { op: 'refund', phase: 'race' });
  if (res.status === 201) admitted.add(1);
  if (res.status === 409) refused.add(1);
  // Shedding under overload (503 payment_busy / overloaded, nothing recorded) is allowed;
  // overrunning capacity or any other answer is not.
  const isShed = res.status === 503 && ['payment_busy', 'overloaded'].includes(res.json().error.code);
  if (isShed) shed.add(1);
  check(res, {
    'admitted, refused by capacity, or shed; nothing else': (r) =>
      r.status === 201 || (r.status === 409 && r.json().error.code === 'insufficient_capacity') || isShed,
  }, { phase: 'race' });
}

export function teardown(data) {
  for (const pid of [data.hot, ...data.payments]) {
    const res = call('GET', `/v1/payments/${pid}`, data.key, undefined, null, { phase: 'reconcile' });
    check(res.json(), { 'fully refunded, never more': (b) => b.payment.refunded === 300 && b.payment.captured === 300 }, { phase: 'reconcile' });
  }
  reconcile([data.key]);
}
