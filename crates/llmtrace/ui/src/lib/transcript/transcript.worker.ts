import { readTranscript } from './stream';
import type { TranscriptEvent } from './types';

const controller = new AbortController();
let acknowledge: (() => void) | undefined;
let started = false;
// Every record waits for the UI to consume it. Download and parsing cannot outrun rendering.
const publish = (event: TranscriptEvent): Promise<void> => new Promise(resolve => {
	acknowledge = resolve;
	self.postMessage(event);
});
self.onmessage = async (event: MessageEvent<{ type: 'start'; id: string } | { type: 'ack' }>) => {
	if (event.data.type === 'ack') { acknowledge?.(); acknowledge = undefined; return; }
	if (started) return;
	started = true;
	try {
		const response = await fetch(`/api/sessions/${encodeURIComponent(event.data.id)}/export.jsonl`, { credentials: 'same-origin', signal: controller.signal });
		if (!response.ok) {
			let message = `Could not load transcript (HTTP ${response.status}).`;
			try { const body = await response.json(); if (typeof body.error === 'string') message = body.error; } catch { /* keep status */ }
			self.postMessage({ type: 'error', message, status: response.status } satisfies TranscriptEvent);
			return;
		}
		if (!response.body) throw new Error('The browser did not provide a response stream.');
		await readTranscript(event.data.id, response.body, controller.signal, publish);
	} catch (error) {
		self.postMessage({ type: 'error', message: error instanceof Error ? error.message : 'Could not read transcript.' } satisfies TranscriptEvent);
	}
};
