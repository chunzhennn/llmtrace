import { ApiError } from '$lib/api/client';

// Small reactive async-resource helper. Tracks loading/error/data for a loader
// function and cancels in-flight requests when reloaded or when the caller
// disposes it. Designed for client-side data loading in the SPA.
export class Resource<T> {
	loading = $state(true);
	error = $state<string | null>(null);
	data = $state<T | undefined>(undefined);

	#loader: (signal: AbortSignal) => Promise<T>;
	#controller: AbortController | undefined;

	constructor(loader: (signal: AbortSignal) => Promise<T>) {
		this.#loader = loader;
	}

	get loaded(): boolean {
		return this.data !== undefined;
	}

	async load(): Promise<void> {
		this.#controller?.abort();
		const controller = new AbortController();
		this.#controller = controller;
		this.loading = true;
		this.error = null;
		try {
			const result = await this.#loader(controller.signal);
			if (controller.signal.aborted) return;
			this.data = result;
		} catch (err) {
			if (controller.signal.aborted) return;
			if (err instanceof DOMException && err.name === 'AbortError') return;
			this.error =
				err instanceof ApiError || err instanceof Error ? err.message : 'Failed to load data';
		} finally {
			if (!controller.signal.aborted) this.loading = false;
		}
	}

	dispose(): void {
		this.#controller?.abort();
	}
}
