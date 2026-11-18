<script lang="ts">
	import { onDestroy } from 'svelte';
	import { noteContext } from '$lib/api/endpoints/query';
	import type { NoteContext as Context } from '$lib/api/types';
	let { id }: { id: string } = $props();
	let context = $state<Context>();
	let error = $state('');
	let busy = $state(false);
	const controller = new AbortController();
	onDestroy(() => controller.abort());
	async function load() {
		if (busy || context) return;
		busy = true; error = '';
		try { context = await noteContext(id, controller.signal); }
		catch (err) { if (!controller.signal.aborted) error = err instanceof Error ? err.message : 'Context unavailable'; }
		finally { busy = false; }
	}
</script>

<details class="mt-2 whitespace-normal" ontoggle={(event) => { if (event.currentTarget.open) void load(); }}>
	<summary class="cursor-pointer text-brand">View source context</summary>
	{#if busy}<p>Loading context…</p>{/if}
	{#if error}<p role="alert">{error}</p><button class="btn" onclick={load}>Retry context</button>{/if}
	{#if context?.status === 'available'}
		<pre class="mt-2 max-w-xl whitespace-pre-wrap break-all text-xs">{context.before}<mark>{context.matched}</mark>{context.after}</pre>
		{#if context.truncated}<p class="text-fg-muted text-xs">Bounded source snippet; open the request for the full captured body.</p>{/if}
	{:else if context}<p class="text-fg-muted">Source context unavailable: {context.status}. The tag and its location remain indexed.</p>{/if}
</details>
