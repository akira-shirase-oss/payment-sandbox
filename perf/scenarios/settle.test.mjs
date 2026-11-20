// The settle contract, against a stubbed 202-then-settled server (the CI 44747097733
// failure shape): classify/outcomeOf are the same decisions perf/scenarios/lib.js drives.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { classify, outcomeOf } from './settle.js';

test('a created or replayed answer is settled immediately', () => {
  assert.deepEqual(classify(201, { operation: { state: 'succeeded' } }), { kind: 'settled' });
  assert.deepEqual(classify(200, {}), { kind: 'settled' });
});

test('a 202 waits on the payment it names', () => {
  const body = { payment: { id: 'p-1' }, operation: { id: 'o-9', state: 'uncertain' } };
  assert.deepEqual(classify(202, body), { kind: 'wait', payment: 'p-1', operation: 'o-9' });
});

test('a 202 without ids and any refusal fail with a named reason', () => {
  assert.deepEqual(classify(202, null), { kind: 'failed', reason: '202' });
  assert.deepEqual(classify(503, {}), { kind: 'failed', reason: '503' });
  assert.deepEqual(classify(0, null), { kind: 'failed', reason: '0' });
});

test('the outcome is read only when the operation reaches a terminal state', () => {
  const page = (state) => ({ operations: [{ id: 'o-9', state }] });
  assert.equal(outcomeOf(page('uncertain'), 'o-9'), null);
  assert.equal(outcomeOf(page('pending'), 'o-9'), null);
  assert.equal(outcomeOf(page('succeeded'), 'o-9'), 'succeeded');
  assert.equal(outcomeOf(page('declined'), 'o-9'), 'declined');
  assert.equal(outcomeOf(page('not_executed'), 'o-9'), 'not_executed');
  assert.equal(outcomeOf({ operations: [] }, 'o-9'), null);
  assert.equal(outcomeOf(null, 'o-9'), null);
});

test('the 202-then-settled shape: wait, then the same operation reports succeeded', () => {
  // What the stubbed server answers: first the 202, then the settled readback page.
  const first = classify(202, { payment: { id: 'p-1' }, operation: { id: 'o-9', state: 'uncertain' } });
  assert.equal(first.kind, 'wait');
  const settled = outcomeOf({ operations: [{ id: 'o-9', state: 'succeeded' }] }, first.operation);
  assert.equal(settled, 'succeeded');
});
