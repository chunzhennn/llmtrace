import requestFixture from '$lib/api/fixtures/request-detail.json';
import sessionFixture from '$lib/api/fixtures/session-detail.json';
import type { ExportHeader, ExportRequest, ExportEnd } from './types';

export const sessionId = sessionFixture.id;
export function record(input: unknown, output: unknown, ordinal = 1, stream = false): ExportRequest {
	const req = JSON.stringify(input);
	const res = stream ? String(output) : JSON.stringify(output);
	return {
		type: 'request',
		request: { ...requestFixture, id: `00000000-0000-0000-0000-${String(ordinal).padStart(12, '0')}`, session_id: sessionId, started_at: '2026-09-22T00:00:00Z' },
		request_body: { status: 'available', encoding: 'utf8', data: req, captured_bytes: new TextEncoder().encode(req).length, truncated: false },
		response_body: { status: 'available', encoding: 'utf8', data: res, captured_bytes: new TextEncoder().encode(res).length, truncated: false },
		message_previews: []
	};
}
export const chat = (content: string) => ({ choices: [{ message: { role: 'assistant', content } }] });
export const sse = (...events: unknown[]) => events.map(event => `data: ${typeof event === 'string' ? event : JSON.stringify(event)}\n\n`).join('');
export function header(count: number): ExportHeader {
	return { type: 'session', schema_version: 1, session: sessionFixture, exported_at: '2026-09-22T00:00:00Z', request_count: count };
}
export function end(count: number): ExportEnd {
	return { type: 'end', session_id: sessionId, export_complete: true, request_count: count, message_preview_count: 0, truncated_body_count: 0, unavailable_body_count: 0, captured_bodies_complete: true };
}
