import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// Components mount in happy-dom, a fast stand-in; the real-browser checks are the
// Playwright journeys (e2e/).
export default defineConfig({
	plugins: [svelte()],
	resolve: { conditions: ['browser'] },
	test: { environment: 'happy-dom', include: ['test/**/*.test.ts'] }
});
