import { describe, expect, it } from 'vitest';
import { jsonLines, readTranscript } from './stream';
import { chat, end, header, record, sessionId } from './test-fixtures';
import type { TranscriptEvent } from './types';

function stream(records: unknown[], chunkSize = 1024) {
	const bytes = new TextEncoder().encode(records.map(r => JSON.stringify(r)).join('\n') + '\n');
	let offset = 0;
	return new ReadableStream<Uint8Array>({ pull(controller) {
		if (offset >= bytes.length) { controller.close(); return; }
		controller.enqueue(bytes.slice(offset, offset + chunkSize)); offset += chunkSize;
	} });
}
const request = (i = 1) => record({ messages: [{ role: 'user', content: 'hi中文🙂' }] }, chat('answer'), i);

describe('streamed transcript integrity and cancellation', () => {
	it('reads split UTF-8 and huge lines exactly once per record', async () => {
		const values = [{ content: '🙂中文'.repeat(10000) }, { end: true }];
		const actual = [];
		for await (const item of jsonLines(stream(values, 137), new AbortController().signal)) actual.push(item);
		expect(actual).toEqual(values);
	});
	it('validates the complete snapshot and streams more than 500 requests', async () => {
		const events: TranscriptEvent[] = [];
		await readTranscript(sessionId, stream([header(503), ...Array.from({ length: 503 }, (_, i) => request(i + 1)), end(503)]), new AbortController().signal, async event => { events.push(event); });
		expect(events).toHaveLength(505);
		expect(events.at(-1)?.type).toBe('end');
		expect(events.filter(e => e.type === 'request')).toHaveLength(503);
	});
	it('waits for each consumer acknowledgement before processing another request', async () => {
		const events: TranscriptEvent[] = [];
		let release!: () => void;
		const gate = new Promise<void>(resolve => { release = resolve; });
		const pending = readTranscript(sessionId, stream([header(2), request(1), request(2), end(2)]), new AbortController().signal, async event => { events.push(event); if (event.type === 'request') await gate; });
		await new Promise(resolve => setTimeout(resolve, 5));
		expect(events.map(e => e.type)).toEqual(['header', 'request']);
		release(); await pending;
		expect(events.at(-1)?.type).toBe('end');
	});
	it.each([
		['missing end', [header(1), request()]],
		['wrong count', [header(2), request(), end(2)]],
		['wrong session', [header(1), { ...request(), request: { ...request().request, session_id: 'other' } }, end(1)]],
		['wrong header', [{ ...header(0), schema_version: 2 }, end(0)]],
		['unexpected trailing record', [header(1), request(), end(1), request(2)]],
		['duplicate request', [header(2), request(), request(), end(2)]]
	])('does not claim completion on %s', async (_name, records) => {
		const events: TranscriptEvent[] = [];
		await expect(readTranscript(sessionId, stream(records), new AbortController().signal, async e => { events.push(e); })).rejects.toThrow();
		expect(events.some(e => e.type === 'end')).toBe(false);
	});
	it('keeps published content on an interrupted download and cancels on navigation', async () => {
		const controller = new AbortController(); const events: TranscriptEvent[] = [];
		await expect(readTranscript(sessionId, stream([header(2), request(1), request(2), end(2)]), controller.signal, async e => { events.push(e); if (e.type === 'request') controller.abort(); })).rejects.toMatchObject({ name: 'AbortError' });
		expect(events.map(e => e.type)).toEqual(['header', 'request']);
	});
});
