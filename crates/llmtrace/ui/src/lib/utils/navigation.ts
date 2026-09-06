/** Preserve list filters on refresh without allowing external Back destinations. */
export function listReturnHref(value: string | null, fallback: string): string {
	if (!value) return fallback;
	try {
		const origin = 'https://llmtrace.invalid';
		const url = new URL(value, origin);
		return url.origin === origin && url.pathname === fallback
			? `${url.pathname}${url.search}`
			: fallback;
	} catch {
		return fallback;
	}
}
