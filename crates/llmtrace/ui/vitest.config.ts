import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

// Standalone Vitest config (kept separate from vite.config.ts so unit tests do
// not pull in the SvelteKit plugin). Only the $lib alias is needed for the
// framework-free util/client tests.
export default defineConfig({
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
