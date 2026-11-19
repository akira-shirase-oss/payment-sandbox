// M3 load scenario: uncertain outcomes under load. Purchases arrive at a fixed rate with
// 80% approving tokens, 10% whose reply the network loses (the server's deadline expires;
// the operation is `uncertain`) and 10% for which the network is unavailable (never
// executed). The server's reconciler runs throughout. At the end, after waiting for it,
// nothing may be left open, the journal must balance, every payment must equal its entries,
// and the network may hold no operation the server does not know.
//
// MODE=full: 300 purchases/s for 3,400 s (about 1,020,000 requests with captures).
// MODE=smoke: 30 purchases/s for 20 s.
import { check, sleep } from 'k6';
import exec from 'k6/execution';
import { Counter } from 'k6/metrics';
import { call, projects, statusThresholds } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
const rate = full ? 300 : 30;
const seconds = full ? 3400 : 20;
const uncertain = new Counter('uncertain_answers');

export const options = {
  setupTimeout: '60s',
  // The teardown waits up to 150 s per project (two) for the reconciler; its limit must be
  // above that wait (the first full run hit 180 s before the gate was checked, D-090).
  teardownTimeout: '330s',
  scenarios: {
    purchases: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: `${seconds}s`,
      preAllocatedVUs: 200,
      maxVUs: 3000,
    },
  },
  thresholds: {
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:reconcile}': ['rate==1'],
    'http_req_duration{path:approve}': ['p(95)<100', 'p(99)<250'],
    ...statusThresholds(['approve', 'uncertain']),
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  return { keys: projects(2, 'uncertain') };
}

function token(n) {
  const r = n % 10;
  if (r === 8) return 'tok_sim_lost_reply';
  if (r === 9) return 'tok_sim_unavailable';
  return 'tok_sim_approve';
}

export default function (data) {
  const n = exec.scenario.iterationInTest;
  const key = data.keys[n % 2];
  const t = token(n);
  const path = t === 'tok_sim_approve' ? 'approve' : 'uncertain';
  const res = call('POST', '/v1/payments', key, { amount: 1000, currency: 'EUR', token: t }, `u-${n}`, { path, phase: 'load' });
  if (path === 'approve') {
    check(res, { 'approved': (r) => r.status === 201 }, { phase: 'load' });
    const pid = res.json().payment.id;
    const cap = call('POST', `/v1/payments/${pid}/captures`, key, { amount: 400 }, `c-${n}`, { path, phase: 'load' });
    check(cap, { 'captured': (r) => r.status === 201 }, { phase: 'load' });
  } else {
    const ok = check(res, { 'honestly uncertain (202)': (r) => r.status === 202 && r.json().operation.state === 'uncertain' }, { phase: 'load' });
    if (ok) uncertain.add(1);
  }
}

export function teardown(data) {
  for (const key of data.keys) {
    let body = null;
    for (let i = 0; i < 150; i++) {
      const res = call('GET', '/v1/reconciliation?network=true', key, undefined, null, { phase: 'reconcile' });
      body = res.status === 200 ? res.json() : null;
      if (body && body.report.open_operations === 0) break;
      sleep(1);
    }
    check(body, {
      'every uncertain operation resolved by the reconciler': (b) => b !== null && b.report.open_operations === 0,
      'journal balanced, payments equal their entries, network fully known': (b) => b !== null && b.clean === true,
    }, { phase: 'reconcile' });
    if (body) console.log(`reconciliation: ${JSON.stringify(body.report)} network=${JSON.stringify(body.network)}`);
  }
}
