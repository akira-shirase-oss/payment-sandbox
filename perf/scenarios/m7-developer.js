// M7 load scenario: the developer product as test stores use it (D-068, D-069). Each
// iteration of `stores` is one store's library-style run, sent exactly as the SDK and the
// CLI send it: an authorization under a fresh key; the same request again (a client retry:
// replayed, still one payment); the same key with another amount (a conflict, never a
// reinterpretation); a capture; a partial refund; a refund past what is left (refused, never
// overrun); the payment read back. One run in ten is declined instead and stops there.
// Every fifth run buys on a third project instead (authorize, capture), whose dataset a
// second, slow scenario resets every 30 s (D-066): a reset under traffic answers 200 or
// `409 epoch_changed`, never a failure; a capture of a payment a reset archived a moment
// earlier answers `410 dataset_archived`; and the project still reconciles.
//
// MODE=full: 100 runs/s for 1,800 s, 5.6 requests a run on average (7 for a purchase, 3 for
// a decline, 2 on the reset project): about 1,008,000 requests.
// MODE=smoke: 20 runs/s for 20 s.
// The correctness gate (teardown): every project's current epoch reconciles with nothing
// open; every run's checks held (one payment per key, the conflict refused, the capacity
// never overrun).
import { check } from 'k6';
import exec from 'k6/execution';
import http from 'k6/http';
import { call, projects, reconcile } from './lib.js';

const full = (__ENV.MODE || 'smoke') === 'full';
const duration = full ? '1800s' : '20s';

// Conflicts, capacity refusals and archived payments are answers the scenario expects.
http.setResponseCallback(http.expectedStatuses({ min: 200, max: 299 }, 409, 410));

export const options = {
  setupTimeout: '120s',
  teardownTimeout: '300s',
  scenarios: {
    stores: {
      executor: 'constant-arrival-rate',
      rate: full ? 100 : 20,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: 100,
      maxVUs: 2000,
      exec: 'store',
    },
    resets: {
      executor: 'constant-arrival-rate',
      rate: 1,
      timeUnit: '30s',
      duration,
      preAllocatedVUs: 1,
      maxVUs: 2,
      exec: 'resetUnderTraffic',
    },
  },
  thresholds: {
    'http_req_failed{phase:load}': ['rate<0.0001'],
    'http_req_duration{op:authorize}': ['p(95)<150', 'p(99)<300'],
    'http_req_duration{op:repeat}': ['p(95)<50', 'p(99)<100'],
    'http_req_duration{op:conflict}': ['p(95)<50', 'p(99)<100'],
    'http_req_duration{op:capture}': ['p(95)<150', 'p(99)<300'],
    'http_req_duration{op:refund}': ['p(95)<150', 'p(99)<300'],
    'http_req_duration{op:overrun}': ['p(95)<100', 'p(99)<200'],
    'http_req_duration{op:read}': ['p(95)<50', 'p(99)<100'],
    'http_req_duration{op:reset}': ['p(95)<250', 'p(99)<500'],
    'checks{phase:load}': ['rate>0.9999'],
    'checks{phase:reset}': ['rate==1'],
    'checks{phase:reconcile}': ['rate==1'],
  },
  summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  return { keys: projects(3, 'developer') };
}

const load = (op) => ({ op, phase: 'load' });

// Letters between the parts, so the key's digits never join into a card-number-like run
// (IdempotencyKey::parse refuses those, card_guard).
function key() {
  return `devvu${exec.vu.idInTest}-it${exec.vu.iterationInScenario}-${Date.now().toString(36)}-a`;
}

export function store(data) {
  const n = exec.scenario.iterationInTest;
  if (n % 5 === 4) return buyDuringResets(data.keys[2]);
  const projectKey = data.keys[n % 2];
  const declined = n % 10 === 3;
  const idem = key();
  const body = { amount: 2500, currency: 'USD', token: declined ? 'tok_sim_decline_insufficient_funds' : 'tok_sim_approve' };
  const created = call('POST', '/v1/payments', projectKey, body, idem, load('authorize'));
  const ok = check(created, {
    'authorized (or declined) once': (r) => r.status === 201 && r.json().operation.state === (declined ? 'declined' : 'succeeded'),
  }, { phase: 'load' });
  if (!ok) return;
  const payment = created.json().payment.id;
  const repeat = call('POST', '/v1/payments', projectKey, body, idem, load('repeat'));
  check(repeat, {
    'a retry replays the same payment': (r) => r.status === 200 && r.json().replayed === true && r.json().payment.id === payment,
  }, { phase: 'load' });
  const conflict = call('POST', '/v1/payments', projectKey, { ...body, amount: 2600 }, idem, load('conflict'));
  check(conflict, {
    'another amount under the key is a conflict': (r) => r.status === 409 && r.json().error.code === 'idempotency_key_conflict',
  }, { phase: 'load' });
  if (declined) return;
  const capture = call('POST', `/v1/payments/${payment}/captures`, projectKey, { amount: 2500 }, `${idem}-capture`, load('capture'));
  check(capture, { captured: (r) => r.status === 201 && r.json().payment.status === 'captured' }, { phase: 'load' });
  const refund = call('POST', `/v1/payments/${payment}/refunds`, projectKey, { amount: 1000 }, `${idem}-refund`, load('refund'));
  check(refund, { 'partly refunded': (r) => r.status === 201 && r.json().payment.refunded === 1000 }, { phase: 'load' });
  const overrun = call('POST', `/v1/payments/${payment}/refunds`, projectKey, { amount: 1501 }, `${idem}-overrun`, load('overrun'));
  check(overrun, {
    'a refund past the capacity is refused': (r) => r.status === 409 && r.json().error.code === 'insufficient_capacity',
  }, { phase: 'load' });
  const read = call('GET', `/v1/payments/${payment}`, projectKey, undefined, null, load('read'));
  check(read, {
    'read back as the journal left it': (r) => r.status === 200 && r.json().payment.refunded === 1000 && r.json().operations.length === 3,
  }, { phase: 'load' });
}

// A purchase on the project whose dataset is being reset: the capture either succeeds or,
// when a reset archived the payment in between, is refused as archived; never a 5xx.
function buyDuringResets(projectKey) {
  const idem = key();
  const created = call('POST', '/v1/payments', projectKey, { amount: 900, currency: 'EUR', token: 'tok_sim_approve' }, idem, load('authorize'));
  if (!check(created, { 'authorized during resets': (r) => r.status === 201 }, { phase: 'load' })) return;
  const payment = created.json().payment.id;
  const capture = call('POST', `/v1/payments/${payment}/captures`, projectKey, { amount: 900 }, `${idem}-capture`, load('capture'));
  check(capture, {
    'captured, or archived by a reset in between': (r) =>
      r.status === 201 || (r.status === 410 && r.json().error.code === 'dataset_archived'),
  }, { phase: 'load' });
}

// The third project's dataset, reset under its own traffic: read its epoch, confirm it.
export function resetUnderTraffic(data) {
  const projectKey = data.keys[2];
  const project = call('GET', '/v1/project', projectKey, undefined, null, { op: 'read', phase: 'reset' });
  if (project.status !== 200) return;
  const { slug, dataset_epoch: epoch } = project.json().project;
  const reset = call('POST', '/v1/project/reset', projectKey, { confirm: slug, epoch }, null, { op: 'reset', phase: 'reset' });
  check(reset, {
    'a reset answers 200 or epoch_changed, never a failure': (r) =>
      r.status === 200 || (r.status === 409 && r.json().error.code === 'epoch_changed'),
  }, { phase: 'reset' });
}

export function teardown(data) {
  // Every project's current epoch reconciles with nothing open (an operation in flight when
  // a reset landed resolved in its own, archived epoch).
  reconcile(data.keys);
}
