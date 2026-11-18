<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { promptTagSettings, savePromptTagSettings } from '$lib/api/endpoints/admin';
	import { ApiError } from '$lib/api/client';
	import type { PromptTagSettings } from '$lib/api/types';

	let saved = $state<PromptTagSettings>();
	let draft = $state('');
	let busy = $state(false);
	let conflict = $state(false);
	let message = $state('');
	let error = $state('');
	const controller = new AbortController();

	async function load() {
		busy = true;
		error = '';
		try {
			saved = await promptTagSettings(controller.signal);
			draft = saved.tags.join(', ');
			conflict = false;
		} catch (err) { error = err instanceof Error ? err.message : 'Could not load tags'; }
		finally { busy = false; }
	}

	async function save() {
		if (!saved || busy || conflict) return;
		busy = true;
		error = message = '';
		try {
			const tags = draft.split(/[\s,]+/).filter(Boolean);
			saved = await savePromptTagSettings({ tags, revision: saved.revision }, controller.signal);
			draft = saved.tags.join(', ');
			message = 'Prompt tags saved.';
		} catch (err) {
			error = err instanceof Error ? err.message : 'Could not save tags';
			conflict = err instanceof ApiError && err.status === 409;
		} finally { busy = false; }
	}

	onMount(() => { void load(); });
	onDestroy(() => controller.abort());
</script>

<section id="prompt-tags" class="card mb-4 p-4" aria-labelledby="prompt-tags-heading">
	<h2 id="prompt-tags-heading" class="mb-2 font-medium">Prompt tags</h2>
	<p class="mb-3 text-sm text-fg-muted">Choose the names to index in captured request text, such as [PROJECT: atlas]. Changes apply immediately. Older requests become searchable as background indexing catches up.</p>
	<form onsubmit={(event) => { event.preventDefault(); void save(); }}>
		<label class="text-sm">Indexed tag names
			<input class="input mt-1" bind:value={draft} oninput={() => message = ''} disabled={!saved || busy} placeholder="PROJECT, priority" />
		</label>
		<p class="mt-1 text-xs text-fg-muted">Use up to 32 case-sensitive names, separated by commas. Leave empty to turn off tag indexing.</p>
		<button class="btn btn-brand mt-3" disabled={busy || !saved || conflict || draft === saved.tags.join(', ')}>Save tags</button>
		{#if !busy && (conflict || (!saved && error))}<button class="btn mt-3" type="button" onclick={load}>Reload saved tags</button>{/if}
	</form>
	{#if error}<p class="mt-2 text-sm text-danger" role="alert">{error}</p>{/if}
	{#if message}<p class="mt-2 text-sm" role="status">{message}</p>{/if}
</section>
