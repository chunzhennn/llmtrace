import { describe, expect, it } from 'vitest';
import { Resource } from './resource.svelte';

function pending<T>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: Error) => void;
	const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
	return { promise, resolve, reject };
}

describe('Resource request ownership', () => {
	it('ignores a late response after switching sessions, even when fetch ignores cancellation', async () => {
		const old = pending<{ id: string }>();
		const current = pending<{ id: string }>();
		let id = 'a';
		const resource = new Resource(() => id === 'a' ? old.promise : current.promise);
		const first = resource.load();
		id = 'b';
		resource.reset();
		const second = resource.load();
		current.resolve({ id: 'b' });
		await second;
		old.resolve({ id: 'a' });
		await first;
		expect(resource.data).toEqual({ id: 'b' });
		expect(resource.loading).toBe(false);
	});

	it('does not append an old message page or publish its error after a reset', async () => {
		const resource = new Resource(async () => ({ id: 'b', messages: ['new'] }));
		resource.data = { id: 'a', messages: ['old'] };
		const page = pending<{ id: string; messages: string[] }>();
		const append = resource.load(() => page.promise);
		resource.reset();
		expect(resource.data).toBeUndefined();
		await resource.load();
		page.reject(new Error('old session page failed'));
		expect(await append).toBeUndefined();
		expect(resource.data).toEqual({ id: 'b', messages: ['new'] });
		expect(resource.error).toBeNull();
	});

	it('aborts on disposal and rejects late results', async () => {
		const response = pending<string>();
		let signal: AbortSignal | undefined;
		const resource = new Resource((value) => { signal = value; return response.promise; });
		const load = resource.load();
		resource.dispose();
		expect(signal?.aborted).toBe(true);
		response.resolve('old');
		await load;
		expect(resource.data).toBeUndefined();
	});
});
