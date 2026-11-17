// A stand-in for the payer's browser, for examples/checkout.mjs in scripts/e2e.sh: opens the
// link the store printed and submits what the page submits when the payer picks a scenario
// (the checkout origin's own page API, with the session's secret from the link's fragment).
// The Playwright journeys drive the real page; this only lets an example run end to end.
// SIMULATED MONEY ONLY.
//
//   node e2e/payer.mjs <checkout link> [scenario, default tok_sim_approve]
const [link, token = 'tok_sim_approve'] = process.argv.slice(2);
const url = new URL(link);
const id = url.pathname.split('/').pop();
const secret = url.hash.slice(1);
const response = await fetch(`${url.origin}/api/sessions/${encodeURIComponent(id)}/attempts`, {
	method: 'POST',
	headers: {
		authorization: `Bearer ${secret}`,
		'content-type': 'application/json',
		'idempotency-key': `payer_${id.replaceAll('-', '')}`.replace(/[0-9]/g, (d) => 'abcdefghij'[Number(d)]),
		origin: url.origin
	},
	body: JSON.stringify({ token })
});
const body = await response.json();
if (!response.ok) {
	console.error(`payer: ${response.status} ${JSON.stringify(body)}`);
	process.exit(1);
}
console.log(`payer: submitted ${token}; the page shows ${body.session.state}`);
