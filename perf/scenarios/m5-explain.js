// M5 load scenario: explaining balances under load. Setup seeds SEED purchases (authorize,
// capture, partial refund: three postings each); then balances and explain pages are read
// at a fixed rate. Teardown walks every account's explanation to its end and requires the
// lines to sum exactly to the balance, then a clean reconciliation.
//
// MODE=full: 33,334 purchases (100,002 postings, the guide's dataset), then 2,500 reads/s for
// 400 s (1,000,000 requests). MODE=smoke: 100 purchases, 200 reads/s for 20 s.
import { check, fail } from 'k6';
import exec from 'k6/execution';
import { call, projects } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
const seed = full ? 33334 : 100;
const rate = full ? 2500 : 200;
const seconds = full ? 400 : 20;
const accounts = ['payer_funding', 'payer_held', 'merchant_pending'];

export const options = {
  setupTimeout: full ? '3600s' : '120s',
  teardownTimeout: '600s',
  scenarios: {
    reads: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: `${seconds}s`,
      preAllocatedVUs: 200,
      maxVUs: 2000,
    },
  },
  thresholds: {
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:gate}': ['rate==1'],
    'http_req_duration{op:balances}': ['p(95)<50', 'p(99)<100'],
    'http_req_duration{op:explain}': ['p(95)<50', 'p(99)<100'],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  const [key] = projects(1, 'explain');
  for (let n = 0; n < seed; n++) {
    const auth = call('POST', '/v1/payments', key, { amount: 1000, currency: 'EUR', token: 'tok_sim_approve' }, `e-${n}`);
    if (auth.status !== 201) fail(`seed authorize ${auth.status}`);
    const pid = auth.json().payment.id;
    call('POST', `/v1/payments/${pid}/captures`, key, { amount: 800 }, `ec-${n}`);
    call('POST', `/v1/payments/${pid}/refunds`, key, { amount: 300 }, `er-${n}`);
  }
  return { key };
}

export default function (data) {
  const n = exec.scenario.iterationInTest;
  if (n % 2 === 0) {
    const res = call('GET', '/v1/balances', data.key, undefined, null, { op: 'balances', phase: 'load' });
    check(res, { 'balances': (r) => r.status === 200 }, { phase: 'load' });
  } else {
    const account = accounts[(n >> 1) % 3];
    const res = call('GET', `/v1/balances/EUR/${account}/explain?limit=100`, data.key, undefined, null, { op: 'explain', phase: 'load' });
    check(res, { 'explained': (r) => r.status === 200 }, { phase: 'load' });
  }
}

export function teardown(data) {
  for (const account of accounts) {
    let cursor = '';
    let sum = 0;
    let balance = null;
    let signedBalance = null;
    for (let page = 0; page < 100000; page++) {
      const res = call('GET', `/v1/balances/EUR/${account}/explain?limit=1000${cursor}`, data.key, undefined, null, { phase: 'gate' });
      const body = res.json();
      balance = body.balance_minor;
      signedBalance = body.explanation.balance_minor;
      for (const line of body.explanation.lines) sum += line.amount_minor;
      const next = body.explanation.next;
      if (next === null) break;
      cursor = `&generation=${next.generation}&as_of=${encodeURIComponent(next.as_of)}&after_seq=${next.after_seq}&running=${next.running}`;
    }
    console.log(`explain ${account}: balance=${balance} lines sum=${sum}`);
    check(sum, { 'the explanation sums to the balance': (s) => s === signedBalance }, { phase: 'gate' });
  }
  const report = call('GET', '/v1/reconciliation', data.key, undefined, null, { phase: 'gate' }).json();
  check(report, { 'reconciliation clean': (r) => r.clean === true }, { phase: 'gate' });
}
