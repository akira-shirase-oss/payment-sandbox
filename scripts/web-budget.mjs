#!/usr/bin/env node
// The web apps' size budgets (ROADMAP M6, D-064): what a first visit downloads, measured
// on the built output. "Initial" is what index.html loads before the app can run: its
// module preloads, its stylesheets and the entry modules; route chunks load after. Sizes
// are bytes, gzip at level 9 (what a compressing proxy would send; the sandbox serves
// identity on loopback). Fonts are counted apart: they load on demand per unicode range.
//
//   node scripts/web-budget.mjs              check the budgets (exit 1 on a miss)
//   node scripts/web-budget.mjs --json       print the measurement as JSON
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { gzipSync } from 'node:zlib';

/** Budgets in gzip bytes (D-064): about 20% over the first measurement (checkout 74.0 /
 *  81.1 / 8.4 KB, dashboard 82.8 / 100.7 / 8.8 KB on 2026-11-17), so growth is a decision. */
const BUDGETS = {
	checkout: { initialJs: 90_000, totalJs: 100_000, css: 11_000 },
	dashboard: { initialJs: 100_000, totalJs: 125_000, css: 11_000 }
};

const root = new URL('..', import.meta.url).pathname;

function files(dir) {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		return statSync(path).isDirectory() ? files(path) : [path];
	});
}

function measure(app) {
	const build = join(root, 'apps', app, 'build');
	const all = files(build);
	const size = (path) => {
		const bytes = readFileSync(path);
		return { raw: bytes.length, gzip: gzipSync(bytes, { level: 9 }).length };
	};
	const sum = (paths) =>
		paths.map(size).reduce((a, b) => ({ raw: a.raw + b.raw, gzip: a.gzip + b.gzip }), { raw: 0, gzip: 0 });
	const html = readFileSync(join(build, 'index.html'), 'utf8');
	const referenced = [...html.matchAll(/(?:href|import\()\s*=?\s*"(\/_app\/[^"]+)"/g)].map((m) => join(build, m[1]));
	const initialJs = [...new Set(referenced.filter((p) => p.endsWith('.js')))];
	const css = all.filter((p) => p.endsWith('.css'));
	const js = all.filter((p) => p.endsWith('.js'));
	const fonts = all.filter((p) => p.endsWith('.woff2'));
	return {
		app,
		html: size(join(build, 'index.html')),
		initial_js: { files: initialJs.length, ...sum(initialJs) },
		total_js: { files: js.length, ...sum(js) },
		css: { files: css.length, ...sum(css) },
		fonts: { files: fonts.length, ...sum(fonts) },
		largest_js: js
			.map((p) => ({ file: relative(build, p), ...size(p) }))
			.sort((a, b) => b.gzip - a.gzip)
			.slice(0, 5)
	};
}

const results = Object.keys(BUDGETS).map(measure);
if (process.argv.includes('--json')) {
	console.log(JSON.stringify({ budgets_gzip_bytes: BUDGETS, results }, null, 2));
	process.exit(0);
}
let missed = false;
for (const r of results) {
	const b = BUDGETS[r.app];
	for (const [label, value, limit] of [
		['initial JS', r.initial_js.gzip, b.initialJs],
		['all JS', r.total_js.gzip, b.totalJs],
		['CSS', r.css.gzip, b.css]
	]) {
		const ok = value <= limit;
		missed ||= !ok;
		console.log(`${ok ? 'ok  ' : 'MISS'} ${r.app.padEnd(9)} ${label.padEnd(10)} ${String(value).padStart(7)} B gzip (budget ${limit})`);
	}
}
if (missed) {
	console.error('web-budget: a budget was missed (D-064); measure, decide, and record before raising it');
	process.exit(1);
}
console.log('web-budget ok');
