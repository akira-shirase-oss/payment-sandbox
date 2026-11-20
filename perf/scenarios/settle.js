// Settling an uncertain money answer — pure logic, no k6 imports, so `node --test` can
// drive it against a stubbed server (settle.test.mjs). SIMULATED MONEY ONLY.
//
// A 202 from a money POST is the server's honest "the network's answer is not known yet":
// the operation identity committed before the network call and only readback resolves it
// (D-015). Setup work must proceed on a settled state — seeding captures and refunds
// behind an unresolved authorize is how a capacity probe aborted with no evidence at all
// (CI run 44747097733, first seed authorize answered 202 and the run ended there).

// Terminal operation states; `pending` and `uncertain` resolve only by readback.
const TERMINAL = new Set(['succeeded', 'declined', 'not_executed']);

// What a POST /v1/payments (or a child operation) response means to a seeder:
//   {kind: 'settled'}  — 201 (or a 200 idempotent replay): proceed.
//   {kind: 'wait'}     — 202: poll the payment until the operation settles.
//   {kind: 'failed'}   — any other status: a named refusal.
export function classify(status, body) {
  if (status === 201 || status === 200) return { kind: 'settled' };
  if (status === 202 && body && body.payment && body.operation) {
    return { kind: 'wait', payment: body.payment.id, operation: body.operation.id };
  }
  return { kind: 'failed', reason: `${status}` };
}

// The operation's terminal state inside a GET /v1/payments/{id} body, or null while it is
// still pending/uncertain (or absent from the page).
export function outcomeOf(body, operationId) {
  const operations = (body && body.operations) || [];
  const op = operations.find((o) => o.id === operationId);
  return op && TERMINAL.has(op.state) ? op.state : null;
}
