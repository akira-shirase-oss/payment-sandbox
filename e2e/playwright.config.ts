import { defineConfig, devices } from '@playwright/test';

// scripts/e2e.sh starts the real server (with its checkout and dashboard origins), the
// simulated network and the development store, and passes their addresses in E2E_*; this
// config only points Chromium at them. One worker: the journeys share one store.
export default defineConfig({
	testDir: 'tests',
	workers: 1,
	fullyParallel: false,
	retries: 0,
	reporter: [['list']],
	timeout: 60_000,
	expect: { timeout: 15_000 },
	outputDir: 'test-results',
	use: {
		trace: 'retain-on-failure',
		// The checkout and the dashboard are separate origins; no base URL is assumed.
		...devices['Desktop Chrome']
	},
	projects: [{ name: 'chromium' }]
});
