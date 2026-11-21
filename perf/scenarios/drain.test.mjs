// The drain contract (m2 run 44878702640): a single shed read during the drain
// window ended the poll loop on `body === null`, and the gate failed on a null
// report — the correctness gate reported a scenario artifact, not the ledger.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { drainReconcile } from './drain.js';

const report = (open) => ({ status: 200, body: { report: { open_operations: open } } });
const refused = { status: 503, body: null };

test('a transient refusal does not end the drain', () => {
  const answers = [refused, refused, report(0)];
  const body = drainReconcile(() => (answers.length ? answers.shift() : report(0)), 5_000, () => {});
  assert.equal(body.report.open_operations, 0);
});

test('the gate sees the last real report when operations stay open', () => {
  const answers = [report(3), refused, refused];
  const body = drainReconcile(() => (answers.length ? answers.shift() : refused), 30, () => {});
  assert.equal(body.report.open_operations, 3);
});

test('a refusal that never ends still fails the gate', () => {
  const body = drainReconcile(() => refused, 30, () => {});
  assert.equal(body, null);
});
