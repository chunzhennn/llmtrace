import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Standalone Vitest config (kept separate from vite.config.ts so unit tests do
// not pull in the SvelteKit plugin). Compile Svelte runes for resource tests and
// resolve the same $lib imports as the application.
export default defineConfig({
	plugins: [svelte({ configFile: false })],
	resolve: {
		alias: {
			$lib: fileURLToPath(new URL('./src/lib', import.meta.url))
		}
	},
	test: {
		environment: 'node',
		include: ['src/**/*.{test,spec}.ts']
	}
});
