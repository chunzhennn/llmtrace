<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import * as sessionsApi from '$lib/api/endpoints/sessions';
	import type { Paginated, SessionSummary } from '$lib/api/types';
	import { formatNumber, formatDateTime, formatDuration } from '$lib/utils/format';
	import type { QueryParams } from '$lib/api/client';

	const DEFAULT_LIMIT = 50;

	function currentParams(): QueryParams {
		const sp = page.url.searchParams;
		const params: QueryParams = {};
		const q = sp.get('q');
		if (q) params.q = q;
		params.limit = DEFAULT_LIMIT;
		params.offset = Number(sp.get('offset') ?? 0);
		return params;
	}

	const list = createResource<Paginated<SessionSummary>>((signal) =>
		sessionsApi.listSessions(currentParams(), signal)
	);

	$effect(() => {
		void page.url.search;
		list.load();
	});

	function submitSearch(event: SubmitEvent) {
		event.preventDefault();
		const form = event.currentTarget as HTMLFormElement;
		const q = String(new FormData(form).get('q') ?? '').trim();
		const params = new URLSearchParams();
		if (q) params.set('q', q);
		goto(`${base}/sessions${params.toString() ? `?${params.toString()}` : ''}`, {
			keepFocus: true,
			noScroll: true
		});
	}

	function setOffset(offset: number) {
		const params = new URLSearchParams(page.url.searchParams);
		if (offset <= 0) params.delete('offset');
		else params.set('offset', String(offset));
		goto(`${base}/sessions?${params.toString()}`, { keepFocus: true, noScroll: true });
	}

	const columns: Column[] = [
		{ label: 'User / session' },
		{ label: 'Requests', align: 'right' },
		{ label: 'Max duration', align: 'right' },
		{ label: 'First seen' },
		{ label: 'Last seen' }
	];

</script>

<svelte:head><title>Sessions · llmtrace</title></svelte:head>

<PageHeader title="Sessions" description="Grouped conversations and their captured requests." />

<form onsubmit={submitSearch} class="card mb-4 flex flex-wrap items-center gap-2 p-3">
	<input
		class="input flex-1"
		type="text"
		name="q"
		aria-label="Search sessions by user or session key"
		placeholder="Search by session key or user…"
		value={page.url.searchParams.get('q') ?? ''}
	/>
	<button type="submit" class="btn btn-brand">
		<Icon name="search" size={16} /> Search
	</button>
	{#if page.url.searchParams.get('q')}
		<a class="btn" href={`${base}/sessions`}>
			<Icon name="x" size={16} /> Clear
		</a>
	{/if}
</form>

<DataTable
	{columns}
	rows={list.data?.items ?? []}
	loading={list.loading}
	error={list.error}
	emptyTitle="No sessions"
	emptyMessage="No sessions matched your search."
	onRetry={() => list.load()}
	skeletonRows={10}
>
	{#snippet row(item: SessionSummary)}
		<td class="px-3 py-2">
			<a class="font-medium hover:underline" style="color: var(--color-brand);" href={`${base}/sessions/${item.id}?from=${encodeURIComponent(page.url.pathname + page.url.search)}`}>
				{item.user_name || item.user_id || `Session ${item.id.slice(0, 8)}`}
			</a>
			{#if item.user_name || item.user_id}
				<div class="text-fg-muted mt-1 font-mono text-xs" title={item.session_key}>Session {item.id.slice(0, 8)}</div>
			{/if}
		</td>
		<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
		<td class="px-3 py-2 text-right tabular-nums">{formatDuration(item.max_duration_ms)}</td>
		<td class="px-3 py-2 whitespace-nowrap">{formatDateTime(item.first_seen)}</td>
		<td class="px-3 py-2 whitespace-nowrap">{formatDateTime(item.last_seen)}</td>
	{/snippet}
</DataTable>

{#if list.data}
	<Pagination
		offset={list.data.page.offset}
		limit={list.data.page.limit}
		count={list.data.items.length}
		hasMore={list.data.page.has_more}
		onChange={setOffset}
	/>
{/if}
