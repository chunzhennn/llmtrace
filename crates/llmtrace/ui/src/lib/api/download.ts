// Helpers for the *.jsonl export endpoints. These return an attachment stream
// rather than JSON, so they bypass the standard client and trigger a browser
// download from the response Blob.

import { API_BASE, ApiError, buildQuery, type QueryParams } from './client';

function filenameFromDisposition(header: string | null, fallback: string): string {
	if (!header) return fallback;
	const match = /filename="?([^"]+)"?/i.exec(header);
	return match?.[1] ?? fallback;
}

function triggerDownload(blob: Blob, filename: string): void {
	const url = URL.createObjectURL(blob);
	const anchor = document.createElement('a');
	anchor.href = url;
	anchor.download = filename;
	document.body.appendChild(anchor);
	anchor.click();
	anchor.remove();
	URL.revokeObjectURL(url);
}

async function runExport(
	method: 'GET' | 'POST',
	path: string,
	fallbackName: string,
	options: { params?: QueryParams; body?: unknown } = {}
): Promise<number> {
	const init: RequestInit = { method, credentials: 'same-origin' };
	if (options.body !== undefined) {
		init.headers = { 'content-type': 'application/json' };
		init.body = JSON.stringify(options.body);
	}

	const response = await fetch(`${API_BASE}${path}${buildQuery(options.params)}`, init);
	if (!response.ok) {
		let message = `export failed with status ${response.status}`;
		try {
			const data = await response.json();
			if (data && typeof data.error === 'string') message = data.error;
		} catch {
			// keep generic message
		}
		throw new ApiError(response.status, message);
	}

	const rowsHeader = response.headers.get('x-llmtrace-export-rows');
	const rows = rowsHeader ? Number(rowsHeader) : Number.NaN;
	const filename = filenameFromDisposition(
		response.headers.get('content-disposition'),
		fallbackName
	);
	const blob = await response.blob();
	triggerDownload(blob, filename);
	return Number.isNaN(rows) ? 0 : rows;
}

export const exportJsonl = {
	requests: (params?: QueryParams) =>
		runExport('GET', '/requests/export.jsonl', 'llmtrace-requests.jsonl', { params }),
	sessionRequests: (id: string, params?: QueryParams) =>
		runExport(
			'GET',
			`/sessions/${id}/requests/export.jsonl`,
			'llmtrace-session-requests.jsonl',
			{ params }
		),
	sessionMessages: (id: string, params?: QueryParams) =>
		runExport(
			'GET',
			`/sessions/${id}/messages/export.jsonl`,
			'llmtrace-session-messages.jsonl',
			{ params }
		),
	auditEvents: (params?: QueryParams) =>
		runExport('GET', '/audit-events/export.jsonl', 'llmtrace-audit-events.jsonl', { params }),
	query: (body: unknown) =>
		runExport('POST', '/query/export.jsonl', 'llmtrace-query.jsonl', { body })
};
