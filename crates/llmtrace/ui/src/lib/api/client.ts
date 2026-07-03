// Central fetch wrapper for the llmtrace JSON API.
//
// All API routes are same-origin under /api. The session is an HttpOnly cookie
// the SPA cannot read, so requests simply send credentials and rely on
// GET /api/auth/me to determine auth status. A registered handler is invoked on
// 401 responses so the auth store can redirect to the login page without
// creating a circular import between this module and the store.

export const API_BASE = '/api';

export class ApiError extends Error {
	status: number;
	retryAfterSecs: number | null;

	constructor(status: number, message: string, retryAfterSecs: number | null = null) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.retryAfterSecs = retryAfterSecs;
	}
}

export type QueryParams = Record<
	string,
	string | number | boolean | null | undefined
>;

let unauthorizedHandler: (() => void) | null = null;

/** Register a callback invoked whenever the API returns 401 (session expired). */
export function setUnauthorizedHandler(handler: (() => void) | null): void {
	unauthorizedHandler = handler;
}

export function buildQuery(params: QueryParams | undefined): string {
	if (!params) return '';
	const search = new URLSearchParams();
	for (const [key, value] of Object.entries(params)) {
		if (value === null || value === undefined) continue;
		if (typeof value === 'string' && value.trim() === '') continue;
		search.set(key, String(value));
	}
	const query = search.toString();
	return query ? `?${query}` : '';
}

interface RequestOptions {
	params?: QueryParams;
	body?: unknown;
	signal?: AbortSignal;
	/** When true a 401 will not trigger the global unauthorized handler. */
	suppressUnauthorized?: boolean;
}

async function parseError(response: Response): Promise<ApiError> {
	let message = `request failed with status ${response.status}`;
	let retryAfter: number | null = null;
	try {
		const data = await response.json();
		if (data && typeof data === 'object') {
			if (typeof data.error === 'string') message = data.error;
			if (typeof data.retry_after_secs === 'number') retryAfter = data.retry_after_secs;
		}
	} catch {
		// non-JSON error body; keep the generic message
	}
	return new ApiError(response.status, message, retryAfter);
}

async function request<T>(
	method: string,
	path: string,
	options: RequestOptions = {}
): Promise<T> {
	const url = `${API_BASE}${path}${buildQuery(options.params)}`;
	const init: RequestInit = {
		method,
		credentials: 'same-origin',
		headers: {},
		signal: options.signal
	};

	if (options.body !== undefined) {
		init.headers = { 'content-type': 'application/json' };
		init.body = JSON.stringify(options.body);
	}

	let response: Response;
	try {
		response = await fetch(url, init);
	} catch (error) {
		if (error instanceof DOMException && error.name === 'AbortError') throw error;
		throw new ApiError(0, 'network error: could not reach the llmtrace API');
	}

	if (response.status === 401) {
		if (!options.suppressUnauthorized) unauthorizedHandler?.();
		throw await parseError(response);
	}

	if (!response.ok) {
		throw await parseError(response);
	}

	if (response.status === 204) return undefined as T;

	const contentType = response.headers.get('content-type') ?? '';
	if (contentType.includes('application/json')) {
		return (await response.json()) as T;
	}
	return (await response.text()) as unknown as T;
}

export const api = {
	get: <T>(path: string, params?: QueryParams, signal?: AbortSignal) =>
		request<T>('GET', path, { params, signal }),
	post: <T>(path: string, body?: unknown, params?: QueryParams, signal?: AbortSignal) =>
		request<T>('POST', path, { body, params, signal }),
	del: <T>(path: string, params?: QueryParams, signal?: AbortSignal) =>
		request<T>('DELETE', path, { params, signal }),
	/** Auth probe that must never trigger the global redirect handler. */
	probe: <T>(path: string, signal?: AbortSignal) =>
		request<T>('GET', path, { suppressUnauthorized: true, signal }),
	postProbe: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
		request<T>('POST', path, { body, suppressUnauthorized: true, signal })
};
