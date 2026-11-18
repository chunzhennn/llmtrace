<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { base } from '$app/paths';
	import { promptTagSettings } from '$lib/api/endpoints/admin';
	import { querySchema, runQuery } from '$lib/api/endpoints/query';
	import { exportJsonl } from '$lib/api/download';
	import type { QuerySchema, StructuredQueryRequest, StructuredQueryResult } from '$lib/api/types';
	import { mergeQueryPage, promptTagQuery } from '$lib/utils/prompt-tags';
	import { toasts } from '$lib/state/toast.svelte';
	import NoteCoverage from './NoteCoverage.svelte';
	import NoteContext from './NoteContext.svelte';

	let schema = $state<QuerySchema>();
	let tags = $state<string[]>([]);
	let tag = $state('');
	let value = $state('');
	let op = $state<'eq' | 'contains' | 'is_not_null'>('eq');
	let role = $state('');
	let since = $state('');
	let until = $state('');
	let busy = $state(false);
	let error = $state('');
	let result = $state<StructuredQueryResult>();
	let lastQuery = $state<StructuredQueryRequest>();
	let resultSignature = $state('');
	let controller: AbortController | undefined;
	let seq = 0;
	let activeSignature = '';
	const schemaController = new AbortController();
	const signature = $derived(JSON.stringify({ tag, value, op, role, since, until }));
	const previous = $derived(!!result && (resultSignature !== signature || !!error));
	const valueSpec = $derived(schema?.datasets.find(d => d.name === 'sessions')?.related_filter_fields?.find(f => f.name === 'prompt_tag.value'));

	onMount(() => {
		const signal = schemaController.signal;
		void Promise.all([querySchema(signal), promptTagSettings(signal)]).then(([data, settings]) => {
			tags = settings.tags;
			tag = tags[0] ?? '';
			schema = data;
		}).catch(err => { if (!signal.aborted) error = err instanceof Error ? err.message : 'Could not load prompt tags'; });
	});
	onDestroy(() => { schemaController.abort(); controller?.abort(); seq++; });
	$effect(() => { if (controller && signature !== activeSignature) cancel(); });
	function cancel() { controller?.abort(); controller = undefined; seq++; busy = false; }

	async function execute(query: StructuredQueryRequest, more = false) {
		cancel(); controller = new AbortController(); const signal = controller.signal;
		const requestSeq = ++seq; activeSignature = signature; busy = true; error = '';
		try {
			const next = await runQuery(query, signal);
			if (requestSeq !== seq || signal.aborted) return;
			result = more && result ? mergeQueryPage(result, next) : next;
			lastQuery = { ...query, cursor: undefined };
			resultSignature = activeSignature;
		} catch (err) { if (requestSeq === seq && !signal.aborted) error = err instanceof Error ? err.message : 'Tag query failed'; }
		finally { if (requestSeq === seq) { busy = false; controller = undefined; } }
	}
	async function search(sessionId?: string) {
		if (!schema || busy) return;
		try { await execute(promptTagQuery(schema, { tag, op, value, role, since, until }, sessionId)); }
		catch (err) { error = err instanceof Error ? err.message : 'Invalid tag query'; }
	}
	async function more() { if (lastQuery && result?.page?.next_cursor && !previous && !busy) await execute({ ...lastQuery, cursor: result.page.next_cursor }, true); }
	async function exportResults() {
		if (!lastQuery || previous || busy) return;
		cancel(); controller = new AbortController(); const signal = controller.signal;
		const requestSeq = ++seq; activeSignature = signature; busy = true;
		try {
			const metadata = await exportJsonl.query(lastQuery, signal);
			if (requestSeq !== seq || signal.aborted) return;
			toasts.success(`Exported this limited result: ${metadata.rows} rows. ${metadata.has_more ? 'More rows exist. ' : ''}Tag coverage: ${metadata.coverage ?? 'unknown'}, index v${metadata.index_version ?? 'unknown'}.`);
		} catch (err) { if (requestSeq === seq && !signal.aborted) error = err instanceof Error ? err.message : 'Export failed'; }
		finally { if (requestSeq === seq) { busy = false; controller = undefined; } }
	}
</script>

<section class="card mb-4" aria-label="Prompt tag search">
	<div class="flex flex-wrap items-center justify-between gap-3 border-b border-border p-4">
		<h2 class="font-medium">Search prompt tags</h2>
		<a class="text-sm text-brand hover:underline" href={`${base}/admin/system#prompt-tags`}>Configure tags</a>
	</div>
	{#if !schema && !error}<p class="p-4">Loading prompt tags…</p>
	{:else if schema && !valueSpec}<p class="p-4" role="status">This server does not support prompt tag queries.</p>
	{:else if schema && !tags.length}<p class="p-4">Configure a tag to search captured request text.</p>
	{:else if schema}
		<form class="flex flex-wrap items-end gap-3 p-4" onsubmit={(event) => { event.preventDefault(); void search(); }}>
			<label class="text-sm">Tag<select class="input mt-1" bind:value={tag}>
				{#each tags as name}<option value={name}>{name}</option>{/each}
			</select></label>
			<label class="text-sm">Match<select class="input mt-1" aria-label="Tag match" bind:value={op}>
				{#if valueSpec?.operators.includes('eq')}<option value="eq">Tag value equals</option>{/if}
				{#if valueSpec?.operators.includes('contains')}<option value="contains">Tag value contains</option>{/if}
				{#if valueSpec?.operators.includes('is_not_null')}<option value="is_not_null">Tag exists</option>{/if}
			</select></label>
			<label class="min-w-40 flex-1 text-sm">Tag value<input class="input mt-1" aria-label="Tag value" bind:value disabled={op === 'is_not_null'} /></label>
			<label class="text-sm">Role<select class="input mt-1" aria-label="Tag role" bind:value={role}><option value="">All input roles</option>{#each ['system', 'developer', 'user', 'assistant', 'tool', 'function'] as name}<option value={name}>{name}</option>{/each}</select></label>
			<label class="text-sm">Request since<input class="input mt-1" aria-label="Tag since" placeholder="2026-11-01T00:00:00Z" bind:value={since} /></label>
			<label class="text-sm">Request until<input class="input mt-1" aria-label="Tag until" placeholder="RFC3339 with timezone" bind:value={until} /></label>
			<button class="btn btn-brand" disabled={busy}>Search tags</button>
			{#if busy}<button type="button" class="btn" onclick={cancel}>Cancel</button>{/if}
		</form>
	{/if}
	{#if error}<p class="p-4 text-danger" role="alert">{error}</p>{/if}
	{#if result}
		{#if previous}<p class="p-4" role="status">Previous query results. Search again with the current conditions.</p><details class="px-4 pb-3 text-xs"><summary>Previous query conditions</summary><pre class="whitespace-pre-wrap">{JSON.stringify(lastQuery, null, 2)}</pre></details>{/if}
		{#if result.search}<NoteCoverage search={result.search} />{/if}
		<div class="flex flex-wrap items-center justify-between gap-2 p-4">
			<p>{result.rows.length} matching {result.dataset === 'sessions' ? (result.rows.length === 1 ? 'session' : 'sessions') : (result.rows.length === 1 ? 'tag occurrence' : 'tag occurrences')} · live results</p>
			<div class="flex gap-2">{#if result.dataset === 'prompt_tags'}<button class="btn" onclick={() => search()} disabled={busy}>Back to matching sessions</button>{/if}<button class="btn" onclick={exportResults} disabled={busy || previous}>Export limited result</button></div>
		</div>
		{#if result.rows.length === 0}<p class="p-4">0 matches in the indexed scope. Check coverage before drawing conclusions.</p>{/if}
		<ul class="divide-y divide-border">
			{#each result.rows as row, i (result.page?.row_ids?.[i] ?? (typeof row.id === 'string' ? row.id : i))}
				<li class="p-4">
					{#if result.dataset === 'sessions' && typeof row.id === 'string'}
						<a class="break-all text-brand hover:underline" title={String(row.session_key ?? row.id)} href={`${base}/sessions/${row.id}`}>{String(row.user_name ?? row.user_id ?? `Session ${row.id.slice(0, 8)}`)}</a>
						<button class="btn ml-3" onclick={() => search(String(row.id))} disabled={busy || previous}>Matching tags</button>
					{:else}
						<p class="whitespace-pre-wrap break-all font-mono text-sm">{String(row.tag_name) + ' · ' + String(row.tag_value ?? '')}</p>
						<p class="text-fg-muted mt-1 text-xs">{String(row.observed_at)} · {String(row.role)} · {String(row.source_path)}</p>
						{#if typeof row.request_id === 'string'}<a class="text-brand mr-3 text-sm" href={`${base}/requests/${row.request_id}`}>Open request</a>{/if}
						{#if typeof row.session_id === 'string'}<a class="text-brand text-sm" href={`${base}/sessions/${row.session_id}/transcript`}>Open transcript</a>{/if}
						{#if typeof row.id === 'string'}<NoteContext id={row.id} />{/if}
					{/if}
				</li>
			{/each}
		</ul>
		{#if result.page?.next_cursor}<div class="p-4"><button class="btn" onclick={more} disabled={busy || previous}>Load more tag results</button></div>{/if}
	{/if}
</section>
