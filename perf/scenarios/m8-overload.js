// M8 load scenario: OVERLOAD, explicitly labelled — the offered rate is deliberately
// above the runner's capacity, and shedding is the expected, measured behavior (D-102).
// What is asserted is the shed's shape, not its absence: every refusal is a named
// 503 carrying Retry-After, refusals answer fast (never the client's ~2 s timeout),
// and the server never returns a 500. A capacity run (scripts/load-capacity.sh) finds
// the rate the machine can carry; this run proves what happens past it.
//
// RATE/SECONDS override the shape. Runs through scripts/load.sh like any scenario.
import { check, fail } from 'k6';
import exec from 'k6/execution';
import { call, projects, statusThresholds } from './lib.js';

const rate = Number(__ENV.RATE || 3000);
const seconds = Number(__ENV.SECONDS || 60);

export const options = {
  setupTimeout: '120s',
  scenarios: {
    flood: {
      executor: 'ramping-arrival-rate',
      startRate: Math.max(10, Math.floor(rate / 4)),
      timeUnit: '1s',
      stages: [
        { target: rate, duration: '10s' },
        { target: rate, duration: `${seconds}s` },
      ],
      preAllocatedVUs: 500,
      maxVUs: 4000,
    },
  },
  thresholds: {
    // The shed shape (all assertable):
    'checks{phase:load}': ['rate>0.9999'],           // every answer is lawful (201 or 503)
    'http_req_duration{expected_response:false}': ['p(99)<2500'], // refusals come back fast
    'op_status{op:authorize,status:500}': ['count<1'], // never an internal error
    'op_status{op:authorize,status:0}': ['count<1'],   // never the client's deadline
    ...statusThresholds(['authorize']),
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  const [key] = projects(1, 'overload');
  return { key };
}

export default function (data) {
  const n = exec.scenario.iterationInTest;
  const res = call('POST', '/v1/payments', data.key, { amount: 1000, currency: 'EUR', token: 'tok_sim_approve' }, `o-${n}`, { op: 'authorize', phase: 'load' });
  check(res, {
    // Overload may be created or refused — nothing else is lawful.
    'lawful': (r) => r.status === 201 || r.status === 503,
    // A refusal must name its retry: the admission contract, not a bare 503.
    'refusal names its retry': (r) => r.status !== 503 || !!r.headers['Retry-After'],
  }, { phase: 'load' });
}

export function teardown(data) {
  if (!data || !data.key) return;
}
