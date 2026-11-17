import adapter from '@sveltejs/adapter-static';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

// A static single-page app the sandbox server serves on the checkout's own origin
// (PS_CHECKOUT_DIR, D-061). One HTML file answers every route; its two inline scripts (the
// theme, SvelteKit's bootstrap) are allowed by hash in the server's policy.
export default defineConfig({
	// Nothing is inlined as a data: URI: the policy allows fonts and images from 'self' only.
	build: { assetsInlineLimit: 0 },
	plugins: [
		// Tailwind CSS v4 before SvelteKit, as its docs require.
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
			},
			adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html' }),
			// No inline style blocks either: the policy is style-src 'self'.
			inlineStyleThreshold: 0
		})
	],
	server: {
		host: '127.0.0.1',
		port: 56471,
		strictPort: true,
		// In development the API is the server's checkout listener.
		proxy: { '/api': 'http://127.0.0.1:56083' }
	},
	resolve: process.env['VITEST'] ? { conditions: ['browser'] } : undefined,
	test: { environment: 'happy-dom', include: ['src/**/*.test.ts'] }
});
