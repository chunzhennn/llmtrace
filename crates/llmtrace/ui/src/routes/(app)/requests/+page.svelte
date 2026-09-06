<script lang="ts">
	import { requestKindLabel } from '$lib/utils/format';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import RequestFilters from '$lib/components/RequestFilters.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as requestsApi from '$lib/api/endpoints/requests';
	import { exportJsonl } from '$lib/api/download';
	import { toasts } from '$lib/state/toast.svelte';
	import type { RequestListResponse, RequestFacets, RequestSummary } from '$lib/api/types';
	import { formatDateTime, formatDuration, formatBytes, truncateMiddle } from '$lib/utils/format';
	import type { QueryParams } from '$lib/api/client';

	const DEFAULT_LIMIT = 50;

	let exporting = $state(false);

	function currentListParams(): QueryParams {
		const sp = page.url.searchParams;
		const params: QueryParams = {};
		for (const key of [
			'q',
			'status',
			'status_class',
			'has_error',
			'upstream_host',
			'model',
			'request_kind',
			'session_id',
			'api_key_hash',
			'since',
			'until',
			'min_duration_ms',
			'max_duration_ms'
		]) {
			const value = sp.get(key);
			if (value) params[key] = value;
		}
		params.limit = Number(sp.get('limit') ?? DEFAULT_LIMIT);
		params.offset = Number(sp.get('offset') ?? 0);
		return params;
	}

	const list = new Resource<RequestListResponse>((signal) =>
		requestsApi.listRequests(currentListParams(), signal)
	);
	const facets = new Resource<RequestFacets>((signal) =>
		requestsApi.requestFacets({ since_hours: 168 }, signal)
	);

	$effect(() => {
		void page.url.search;
		list.load();
	});

	$effect(() => {
		if (!facets.loaded && !facets.loading) facets.load();
	});

	const activeFilterCount = $derived(
		[...page.url.searchParams.keys()].filter((k) => k !== 'offset' && k !== 'limit').length
	);

	function applyFilters(search: string) {
		goto(`${base}/requests${search ? `?${search}` : ''}`, { keepFocus: true, noScroll: true });
	}

	function resetFilters() {
		goto(`${base}/requests`, { noScroll: true });
	}

	function setOffset(offset: number) {
		const params = new URLSearchParams(page.url.searchParams);
		if (offset <= 0) params.delete('offset');
		else params.set('offset', String(offset));
		goto(`${base}/requests?${params.toString()}`, { keepFocus: true, noScroll: true });
	}

	async function runExport() {
		exporting = true;
		try {
			const params = currentListParams();
			const rows = await exportJsonl.requests(params);
			toasts.success(`Exported ${rows} request${rows === 1 ? '' : 's'}.`);
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Export failed.');
		} finally {
			exporting = false;
		}
	}

	const columns: Column[] = [
		{ label: 'Started' },
		{ label: 'Status' },
		{ label: 'Method' },
		{ label: 'Request' },
		{ label: 'Model' },
		{ label: 'Upstream' },
		{ label: 'Duration', align: 'right' },
		{ label: 'Size', align: 'right' }
	];
</script>

<svelte:head><title>Requests · llmtrace</title></svelte:head>

<PageHeader title="Requests" description="Explore captured proxy requests with filters, facets, and export.">
	{#snippet actions()}
		<button type="button" class="btn" onclick={runExport} disabled={exporting}>
			<Icon name="download" size={16} />
			{exporting ? 'Exporting…' : 'Export page (JSONL)'}
		</button>
	{/snippet}
</PageHeader>

{#key page.url.search}
	<RequestFilters
		params={page.url.searchParams}
		facets={facets.data}
		{activeFilterCount}
		onApply={applyFilters}
		onReset={resetFilters}
	/>
{/key}

<DataTable
	{columns}
	rows={list.data?.items ?? []}
	loading={list.loading}
	error={list.error}
	emptyTitle="No requests match"
	emptyMessage="Try widening your filters or time range."
	onRetry={() => list.load()}
	skeletonRows={10}
>
	{#snippet row(item: RequestSummary)}
		<td class="px-3 py-2 whitespace-nowrap">
			<a class="font-medium hover:underline" style="color: var(--color-brand);" href={`${base}/requests/${item.id}?from=${encodeURIComponent(page.url.pathname + page.url.search)}`}>
				{formatDateTime(item.started_at)}
			</a>
		</td>
		<td class="px-3 py-2"><StatusPill status={item.status} error={item.error} /></td>
		<td class="px-3 py-2 font-mono text-xs">{item.method}</td>
		<td class="px-3 py-2 max-w-[22rem]">
			<a class="block truncate font-mono text-xs hover:underline" style="color: var(--color-brand);" href={`${base}/requests/${item.id}?from=${encodeURIComponent(page.url.pathname + page.url.search)}`} title={item.original_uri}>{truncateMiddle(item.original_uri, 60)}</a>
			<span class="text-fg-muted text-xs">{requestKindLabel(item.request_kind)}</span>
		</td>
		<td class="px-3 py-2 whitespace-nowrap">{item.model ?? '—'}</td>
		<td class="px-3 py-2 max-w-[12rem] truncate" title={item.upstream_host ?? ''}>{item.upstream_host ?? '—'}</td>
		<td class="px-3 py-2 text-right tabular-nums">{formatDuration(item.duration_ms)}</td>
		<td class="px-3 py-2 text-right tabular-nums text-xs">{formatBytes(item.bytes_in + item.bytes_out)}</td>
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
