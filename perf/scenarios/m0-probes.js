// M0 load scenario: the service skeleton under offered load. Liveness (no I/O) and
// readiness (one PostgreSQL round trip through the bounded pool), 4:1, at a fixed arrival
// rate, so latency is measured against offered load rather than closed-loop concurrency.
//
// MODE=full: 1,000,000 requests (10,000/s for 100 s). MODE=smoke: 20 s at 1,000/s.
// SIMULATED MONEY ONLY: this touches no payment path and no network beyond loopback.
import http from 'k6/http';
import { check } from 'k6';

const BASE = __ENV.BASE_URL || 'http://127.0.0.1:56090';
const full = (__ENV.MODE || 'smoke') === 'full';
const rate = full ? 10000 : 1000;
const duration = full ? '100s' : '20s';

export const options = {
  discardResponseBodies: true,
  scenarios: {
    probes: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: 200,
      maxVUs: 1000,
    },
  },
  thresholds: {
    'http_req_failed': ['rate<0.0001'],
    'http_req_duration{probe:healthz}': ['p(99)<25'],
    'http_req_duration{probe:readyz}': ['p(99)<100'],
    'dropped_iterations': [`count<${Math.ceil(rate * (full ? 100 : 20) * 0.001)}`],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export default function () {
  const ready = __ITER % 5 === 4;
  const path = ready ? '/readyz' : '/healthz';
  const res = http.get(`${BASE}${path}`, { tags: { probe: ready ? 'readyz' : 'healthz' } });
  check(res, {
    'status 200': (r) => r.status === 200,
    'labelled simulated': (r) => r.headers['Sandbox-Simulated'] === 'true',
  });
}
