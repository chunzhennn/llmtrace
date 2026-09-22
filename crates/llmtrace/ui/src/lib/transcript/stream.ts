import type { SessionExportRecord } from '$lib/api/types';
import type { TranscriptEvent } from './types';
import { parseRequest } from './parse';
import { TranscriptHistory } from './dedupe';

/** One JSONL record at a time. Joining chunk fragments once avoids quadratic copying of large bodies. */
export async function* jsonLines(stream: ReadableStream<Uint8Array>, signal: AbortSignal): AsyncGenerator<unknown> {
	const reader = stream.getReader();
	const decoder = new TextDecoder('utf-8', { fatal: true });
	let fragments: string[] = [];
	const cancel = () => { void reader.cancel().catch(() => {}); };
	signal.addEventListener('abort', cancel, { once: true });
	try {
		while (true) {
			signal.throwIfAborted();
			const { done, value } = await reader.read();
			signal.throwIfAborted();
			const text = decoder.decode(value, { stream: !done });
			let start = 0;
			for (let end; (end = text.indexOf('\n', start)) >= 0;) {
				fragments.push(text.slice(start, end));
				const line = fragments.join(''); fragments = [];
				if (line.trim()) yield JSON.parse(line);
				start = end + 1;
			}
			if (start < text.length) fragments.push(text.slice(start));
			if (done) break;
		}
		const tail = fragments.join('');
		if (tail.trim()) yield JSON.parse(tail);
	} finally {
		signal.removeEventListener('abort', cancel);
		await reader.cancel().catch(() => {});
		reader.releaseLock();
	}
}

export async function readTranscript(
	id: string, stream: ReadableStream<Uint8Array>, signal: AbortSignal,
	publish: (event: TranscriptEvent) => Promise<void>
): Promise<void> {
	const history = new TranscriptHistory();
	let expected: number | undefined;
	let count = 0;
	const seen = new Set<string>();
	let previous: { startedAt: string; id: string } | undefined;
	let footer: Extract<SessionExportRecord, { type: 'end' }> | undefined;
	for await (const raw of jsonLines(stream, signal)) {
		signal.throwIfAborted();
		if (!raw || typeof raw !== 'object') throw new Error('Invalid transcript export record.');
		const record = raw as SessionExportRecord;
		if (footer) throw new Error('Unexpected records after the transcript end marker.');
		if (expected === undefined) {
			if (record.type !== 'session' || record.schema_version !== 1 || record.session.id !== id
				|| !Number.isSafeInteger(record.request_count) || record.request_count < 0) throw new Error('Invalid transcript session header.');
			expected = record.request_count;
			await publish({ type: 'header', header: record });
		} else if (record.type === 'request') {
			if (record.request.session_id !== id) throw new Error('The transcript contains a request from another session.');
			const startedAt = record.request.started_at;
			if (!Number.isFinite(Date.parse(startedAt)) || seen.has(record.request.id)) throw new Error('Invalid or duplicate transcript request.');
			seen.add(record.request.id);
			// Timestamps in this export use one Rust serializer, so their lexical order is stable
			// except for optional fractional zeros. Compare their epoch values for that case.
			if (previous && (Date.parse(startedAt) < Date.parse(previous.startedAt)
				|| (startedAt === previous.startedAt && record.request.id <= previous.id))) throw new Error('Transcript requests are out of order.');
			previous = { startedAt, id: record.request.id };
			count++;
			if (count > expected) throw new Error('Transcript request count exceeds its snapshot.');
			const parsed = parseRequest(record);
			await publish({ type: 'request', ...history.add(record, parsed) });
		} else if (record.type === 'end') {
			if (record.session_id !== id || record.export_complete !== true || count !== record.request_count || count !== expected) throw new Error('Transcript export is incomplete. Reload to try again.');
			footer = record;
		} else throw new Error('Unexpected transcript export record.');
	}
	signal.throwIfAborted();
	if (!footer) throw new Error('Transcript download ended before completion. Reload to try again.');
	await publish({ type: 'end', end: footer });
}
