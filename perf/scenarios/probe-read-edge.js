// D-099 probe: while `writes` flood the API listener, the same read stream runs twice —
// `reads_main` shares the write listener (the "before"), `reads_edge` uses the read edge
// (PS_READ_LISTEN, the "after"). The measurement is the p99 of each read stream and the
// status mix: reads may be refused fast (named 503 + Retry-After) but never wait ~2 s.
//
// RATE/READ_RATE/SECONDS override the shape; run it through scripts/load.sh like any
// scenario. RATE defaults to the CI's per-core arrival rate on a 12-core machine.
import { check } from 'k6';
import exec from 'k6/execution';
import { call, projects, seedSettled, statusThresholds, BASE } from './lib.js';

const edge = __ENV.READ_URL || BASE;
const rate = Number(__ENV.RATE || 855);
const readRate = Number(__ENV.READ_RATE || 25);
const seconds = Number(__ENV.SECONDS || 60);
const accounts = ['payer_funding', 'payer_held', 'merchant_pending'];

export const options = {
  setupTimeout: '300s',
  scenarios: {
    writes: {
      executor: 'ramping-arrival-rate',
      startRate: 10,
      timeUnit: '1s',
      stages: [
        { target: rate, duration: '15s' },
        { target: rate, duration: `${seconds}s` },
      ],
      preAllocatedVUs: 500,
      maxVUs: 4000,
      exec: 'writes',
    },
    reads_main: {
      executor: 'constant-arrival-rate',
      rate: readRate,
      timeUnit: '1s',
      duration: `${seconds + 15}s`,
      preAllocatedVUs: 50,
      maxVUs: 300,
      exec: 'readsMain',
    },
    reads_edge: {
      executor: 'constant-arrival-rate',
      rate: readRate,
      timeUnit: '1s',
      duration: `${seconds + 15}s`,
      preAllocatedVUs: 50,
      maxVUs: 300,
      exec: 'readsEdge',
    },
  },
  thresholds: {
    // Threshold-referenced so the per-op latencies survive --summary-export (no gate).
    'http_req_duration{op:authorize}': ['p(99)>-1'],
    'http_req_duration{op:balances_main}': ['p(99)>-1'],
    'http_req_duration{op:balances_edge}': ['p(99)>-1'],
    'http_req_duration{op:explain_main}': ['p(99)>-1'],
    'http_req_duration{op:explain_edge}': ['p(99)>-1'],
    // Refused reads alone: where they wait is the D-099 question — the main listener's
    // ~2 s place wait vs the read edge's ~100 ms one and the bounded read-permit wait.
    'http_req_duration{op:balances_main,expected_response:false}': ['p(99)>-1'],
    'http_req_duration{op:balances_edge,expected_response:false}': ['p(99)>-1'],
    'http_req_duration{op:explain_main,expected_response:false}': ['p(99)>-1'],
    'http_req_duration{op:explain_edge,expected_response:false}': ['p(99)>-1'],
    'http_req_duration{op:authorize,expected_response:false}': ['p(99)>-1'],
    ...statusThresholds(['authorize', 'balances_main', 'explain_main', 'balances_edge', 'explain_edge']),
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  const [key] = projects(1, 'readedge');
  for (let n = 0; n < 200; n++) {
    const auth = call('POST', '/v1/payments', key, { amount: 1000, currency: 'EUR', token: 'tok_sim_approve' }, `s-${n}`);
    seedSettled(auth, key, 'seed authorize');
    const pid = auth.json().payment.id;
    seedSettled(call('POST', `/v1/payments/${pid}/captures`, key, { amount: 800 }, `sc-${n}`), key, 'seed capture');
    seedSettled(call('POST', `/v1/payments/${pid}/refunds`, key, { amount: 300 }, `sr-${n}`), key, 'seed refund');
  }
  return { key };
}

function read(base, side, key, n) {
  if (n % 2 === 0) {
    return call('GET', '/v1/balances', key, undefined, null, { op: `balances_${side}` }, base);
  }
  const account = accounts[(n >> 1) % 3];
  return call('GET', `/v1/balances/EUR/${account}/explain?limit=100`, key, undefined, null, { op: `explain_${side}` }, base);
}

export function writes(data) {
  const n = exec.scenario.iterationInTest;
  const res = call('POST', '/v1/payments', data.key, { amount: 1000, currency: 'EUR', token: 'tok_sim_approve' }, `w-${n}`, { op: 'authorize' });
  check(res, { 'authorized': (r) => r.status === 201 });
}

export function readsMain(data) {
  const res = read(BASE, 'main', data.key, exec.scenario.iterationInTest);
  check(res, { 'read main': (r) => r.status === 200 });
}

export function readsEdge(data) {
  const res = read(edge, 'edge', data.key, exec.scenario.iterationInTest);
  check(res, { 'read edge': (r) => r.status === 200 });
}
