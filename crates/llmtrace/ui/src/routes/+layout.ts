// The entire dashboard is a client-rendered single-page app served statically
// by the llmtrace backend under /ui. There is no server runtime for the UI, so
// disable SSR and prerendering globally.
export const ssr = false;
export const prerender = false;
export const trailingSlash = 'ignore';
