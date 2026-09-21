import { onDestroy } from 'svelte';

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

	async load(loader = this.#loader): Promise<T | undefined> {
		this.#controller?.abort();
		const controller = new AbortController();
		this.#controller = controller;
		this.loading = true;
		this.error = null;
		try {
			const result = await loader(controller.signal);
			if (controller.signal.aborted) return;
			this.data = result;
			return result;
		} catch (err) {
			if (controller.signal.aborted) return;
			if (err instanceof DOMException && err.name === 'AbortError') return;
			this.error = err instanceof Error ? err.message : 'Failed to load data';
		} finally {
			if (!controller.signal.aborted) this.loading = false;
		}
	}

	reset(): void {
		this.dispose();
		this.data = undefined;
		this.error = null;
		this.loading = true;
	}

	dispose(): void {
		this.#controller?.abort();
	}
}

// Component-owned resources always cancel on unmount. A key also clears and
// reloads detail data on navigation, keeping IDs and displayed content together.
export function createResource<T>(
	loader: (signal: AbortSignal) => Promise<T>,
	key?: () => unknown
): Resource<T> {
	const resource = new Resource(loader);
	onDestroy(() => resource.dispose());
	if (key) {
		$effect(() => {
			key();
			resource.reset();
			void resource.load();
		});
	}
	return resource;
}
