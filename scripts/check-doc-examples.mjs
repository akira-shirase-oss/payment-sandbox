// Every example a document shows is the file CI runs, byte for byte (the interface
// standard: "examples that run as written"). A document marks one with
//   <!-- example: examples/file -->
// followed by a fenced code block; that block must equal the file. SIMULATED MONEY ONLY.
import { readFileSync } from 'node:fs';

const documents = ['docs/DEVELOPERS.md', 'docs/OPERATING.md'];
let failures = 0;
let checked = 0;
for (const document of documents) {
	let text;
	try {
		text = readFileSync(document, 'utf8');
	} catch {
		continue;
	}
	const marker = /<!-- example: (\S+) -->\n```[a-z]*\n([\s\S]*?)```/g;
	for (const [, path, block] of text.matchAll(marker)) {
		checked += 1;
		if (readFileSync(path, 'utf8') !== block) {
			console.error(`${document}: the example shown for ${path} differs from the file; copy the file into the document.`);
			failures += 1;
		}
	}
}
if (checked === 0) {
	console.error('no examples found: the markers are missing');
	process.exit(1);
}
if (failures > 0) process.exit(1);
console.log(`doc examples ok: ${checked} examples match their files`);
