import { describe, it, expect, vi, afterEach } from 'vitest';
import { api, ApiError, buildQuery, setUnauthorizedHandler } from './client';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' }
	});
}

afterEach(() => {
	vi.restoreAllMocks();
	setUnauthorizedHandler(null);
});

describe('buildQuery', () => {
	it('omits nullish and empty string params', () => {
		expect(buildQuery({ a: 1, b: null, c: undefined, d: '', e: 'x' })).toBe('?a=1&e=x');
	});
	it('returns an empty string for no params', () => {
		expect(buildQuery(undefined)).toBe('');
		expect(buildQuery({})).toBe('');
	});
	it('serializes booleans', () => {
		expect(buildQuery({ include_expired: true })).toBe('?include_expired=true');
	});
});

describe('api request 401 handling', () => {
	it('invokes the unauthorized handler and throws ApiError on 401', async () => {
		const handler = vi.fn();
		setUnauthorizedHandler(handler);
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ error: 'unauthorized' }, 401)));

		await expect(api.get('/stats')).rejects.toBeInstanceOf(ApiError);
		expect(handler).toHaveBeenCalledOnce();
	});

	it('does not invoke the handler for probe requests', async () => {
		const handler = vi.fn();
		setUnauthorizedHandler(handler);
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ error: 'nope' }, 401)));

		await expect(api.probe('/auth/me')).rejects.toBeInstanceOf(ApiError);
		expect(handler).not.toHaveBeenCalled();
	});
});

describe('api request error and success bodies', () => {
	it('parses error message and retry_after_secs from the body', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(jsonResponse({ error: 'slow down', retry_after_secs: 12 }, 429))
		);
		try {
			await api.get('/stats');
			throw new Error('expected rejection');
		} catch (err) {
			expect(err).toBeInstanceOf(ApiError);
			const apiErr = err as ApiError;
			expect(apiErr.status).toBe(429);
			expect(apiErr.message).toBe('slow down');
			expect(apiErr.retryAfterSecs).toBe(12);
		}
	});

	it('returns parsed JSON on success', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse({ total: 7 })));
		const data = await api.get<{ total: number }>('/stats');
		expect(data).toEqual({ total: 7 });
	});

	it('maps network failures to a status-0 ApiError', async () => {
		vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('boom')));
		await expect(api.get('/stats')).rejects.toMatchObject({ status: 0 });
	});

	it('propagates AbortError without wrapping', async () => {
		const abort = new DOMException('aborted', 'AbortError');
		vi.stubGlobal('fetch', vi.fn().mockRejectedValue(abort));
		await expect(api.get('/stats')).rejects.toBe(abort);
	});
});
