// A merchant's page on its own loopback origin, for the embedding journey: it frames the
// checkout URLs it is given (?allowed=…&refused=…) and records the messages it receives.
// A real listener, because Chromium's local-network checks refuse to let a page that did
// not come from this machine frame a loopback one. Usage: node merchant.mjs <port>
import { createServer } from 'node:http';

const port = Number(process.argv[2] ?? 56465);
const page = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Merchant</title></head><body>
<h1>A test merchant</h1>
<iframe id="allowed" title="allowed" width="420" height="720"></iframe>
<iframe id="refused" title="refused" width="420" height="200"></iframe>
<script>
window.messages = [];
addEventListener('message', (e) => window.messages.push({ origin: e.origin, data: e.data }));
const q = new URLSearchParams(location.search);
for (const id of ['allowed', 'refused']) if (q.get(id)) document.getElementById(id).src = q.get(id);
</script></body></html>`;

createServer((request, response) => {
	response.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
	response.end(page);
}).listen(port, '127.0.0.1');
