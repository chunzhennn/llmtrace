import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig, type ProxyOptions } from 'vite';

// The llmtrace backend enforces a same-origin check on unsafe /api requests
// (Origin/Referer must match server.public_url). During local development the
// Vite dev server proxies API traffic to the backend and rewrites the Origin
// header so login and other mutations are accepted.
const backend = process.env.LLMTRACE_BACKEND ?? 'http://127.0.0.1:3000';

const proxyPaths = ['/api', '/healthz', '/readyz'];

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			adapter: adapter({
				fallback: 'index.html',
				strict: false
			}),
			paths: {
				base: '/ui'
			}
		})
	],
	server: {
		proxy: Object.fromEntries(
			proxyPaths.map((path): [string, ProxyOptions] => [
				path,
				{
					target: backend,
					changeOrigin: true,
					ws: true,
					configure: (proxy) => {
						proxy.on('proxyReq', (proxyReq) => {
							proxyReq.setHeader('origin', backend);
						});
					}
				}
			])
		)
	}
});
