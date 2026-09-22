import { api, ApiError } from '$lib/api/client';
import type { TranscriptEvent } from './types';

export function streamSessionTranscript(id: string, signal: AbortSignal, onEvent: (event: TranscriptEvent) => void): Promise<void> {
	signal.throwIfAborted();
	return new Promise((resolve, reject) => {
		const worker = new Worker(new URL('./transcript.worker.ts', import.meta.url), { type: 'module' });
		const cleanup = () => { signal.removeEventListener('abort', abort); worker.terminate(); };
		const abort = () => { cleanup(); reject(new DOMException('Transcript load cancelled', 'AbortError')); };
		signal.addEventListener('abort', abort, { once: true });
		worker.onerror = () => { cleanup(); reject(new Error('The transcript worker failed. Reload to try again.')); };
		worker.onmessage = ({ data }: MessageEvent<TranscriptEvent>) => {
			if (signal.aborted) return;
			try {
				if (data.type === 'error') {
					if (data.status === 401) void api.get('/auth/me').catch(() => {});
					throw new ApiError(data.status ?? 0, data.message);
				}
				onEvent(data);
				if (data.type === 'end') { cleanup(); resolve(); }
				else worker.postMessage({ type: 'ack' });
			} catch (error) { cleanup(); reject(error); }
		};
		worker.postMessage({ type: 'start', id });
	});
}
