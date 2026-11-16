// M1 load scenario: authenticated reads. Every request hashes its sk_sim_ key and resolves
// it in PostgreSQL (no cache: a revoked key must fail on its next request), then reads the
// project. Two projects, two keys each, fixed arrival rate.
//
// MODE=full: 1,000,000 requests (5,000/s for 200 s). MODE=smoke: 20 s at 500/s.
import http from 'k6/http';
import { check, fail } from 'k6';

const BASE = __ENV.BASE_URL || 'http://127.0.0.1:56090';
const BOOTSTRAP = __ENV.BOOTSTRAP;
const full = (__ENV.MODE || 'smoke') === 'full';
const rate = full ? 5000 : 500;
const seconds = full ? 200 : 20;

export const options = {
  discardResponseBodies: true,
  setupTimeout: '60s',
  scenarios: {
    reads: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: `${seconds}s`,
      preAllocatedVUs: 200,
      maxVUs: 1000,
    },
  },
  thresholds: {
    'http_req_failed': ['rate<0.0001'],
    'http_req_duration': ['p(95)<25', 'p(99)<50'],
    'checks': ['rate>0.9999'],
    'dropped_iterations': [`count<${Math.ceil(rate * seconds * 0.001)}`],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

function post(path, token, body) {
  return http.post(`${BASE}${path}`, JSON.stringify(body), {
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    responseType: 'text',
  });
}

export function setup() {
  if (!BOOTSTRAP) fail('BOOTSTRAP is required');
  const run = `${Date.now()}`;
  const keys = [];
  for (const name of ['a', 'b']) {
    const res = post('/v1/projects', BOOTSTRAP, { slug: `load-${name}-${run}`, name: `Load ${name}` });
    if (res.status !== 201) fail(`project: ${res.status} ${res.body}`);
    const body = res.json();
    const admin = body.api_key.secret;
    const reader = post('/v1/keys', admin, { label: 'load reader', scopes: ['read'] });
    if (reader.status !== 201) fail(`key: ${reader.status}`);
    keys.push({ key: admin, project: body.project.id });
    keys.push({ key: reader.json().secret, project: body.project.id });
  }
  return { keys };
}

export default function (data) {
  const pick = data.keys[__ITER % data.keys.length];
  const res = http.get(`${BASE}/v1/project`, { headers: { authorization: `Bearer ${pick.key}` } });
  check(res, { 'status 200': (r) => r.status === 200 });
}
