// The reconcile drain's polling policy, extracted so node --test can drive it —
// lib.js itself imports k6, which the test runner cannot load.
// A non-200 poll is a shed read, not the end of the drain: the window exists to
// wait out the post-offer backlog, and one refusal inside it must not falsify the
// correctness gate (m2 run 44878702640 — the first teardown poll shed, the gate
// failed on a null report). The loop keeps the last real report and ends only on
// a settled one or the deadline.
export function drainReconcile(fetchReport, drainMs, sleep) {
  let body = null;
  const deadline = Date.now() + drainMs;
  for (;;) {
    const res = fetchReport();
    if (res.status === 200 && res.body !== null) body = res.body;
    if ((body !== null && body.report.open_operations === 0) || Date.now() >= deadline) break;
    sleep(2);
  }
  return body;
}
