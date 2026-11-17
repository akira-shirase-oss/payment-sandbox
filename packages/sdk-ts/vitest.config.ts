import { defineConfig } from 'vitest/config';

// Unit tests (test/unit) run anywhere; the live tests (test/live) need the real server,
// network and store that scripts/e2e.sh starts, and its E2E_* variables.
export default defineConfig({
	test: { environment: 'node', include: ['test/**/*.test.ts'], testTimeout: 60_000 }
});
