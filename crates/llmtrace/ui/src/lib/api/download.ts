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
	options: { params?: QueryParams; body?: unknown; signal?: AbortSignal; onQueryMetadata?: (metadata: QueryExportMetadata) => void } = {}
): Promise<number> {
	const init: RequestInit = { method, credentials: 'same-origin', signal: options.signal };
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
	options.signal?.throwIfAborted();
	options.onQueryMetadata?.({ rows: Number.isNaN(rows) ? 0 : rows,
		has_more: response.headers.get('x-llmtrace-query-has-more') === 'true',
		coverage: response.headers.get('x-llmtrace-query-coverage'),
		index_version: response.headers.get('x-llmtrace-query-index-version') });
	triggerDownload(blob, filename);
	return Number.isNaN(rows) ? 0 : rows;
}

export interface QueryExportMetadata { rows: number; has_more: boolean; coverage: string | null; index_version: string | null }

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
	query: async (body: unknown, signal?: AbortSignal): Promise<QueryExportMetadata> => {
		let metadata: QueryExportMetadata = { rows: 0, has_more: false, coverage: null, index_version: null };
		await runExport('POST', '/query/export.jsonl', 'llmtrace-query.jsonl', { body, signal, onQueryMetadata: (value) => metadata = value });
		return metadata;
	}
};
