<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import Field from '$lib/components/Field.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import * as auditApi from '$lib/api/endpoints/audit';
	import { exportJsonl } from '$lib/api/download';
	import { toasts } from '$lib/state/toast.svelte';
	import type { AuditEventsResponse, AuditSummary, AuditEvent } from '$lib/api/types';
	import type { QueryParams } from '$lib/api/client';
	import { formatNumber, formatDateTime } from '$lib/utils/format';

	const LIMIT = 50;

	function currentParams(): QueryParams {
		const sp = page.url.searchParams;
		const params: QueryParams = { limit: LIMIT, offset: Number(sp.get('offset') ?? 0) };
		const et = sp.get('event_type');
		const uid = sp.get('user_id');
		if (et) params.event_type = et;
		if (uid) params.user_id = uid;
		return params;
	}

	const events = createResource<AuditEventsResponse>((signal) => auditApi.listAuditEvents(currentParams(), signal));
	const summary = createResource<AuditSummary>((signal) => auditApi.auditSummary({ since_hours: 168 }, signal));

	$effect(() => {
		void page.url.search;
		events.load();
	});

	onMount(() => summary.load());

	function applyFilters(event: SubmitEvent) {
		event.preventDefault();
		const form = new FormData(event.currentTarget as HTMLFormElement);
		const params = new URLSearchParams();
		const et = String(form.get('event_type') ?? '').trim();
		const uid = String(form.get('user_id') ?? '').trim();
		if (et) params.set('event_type', et);
		if (uid) params.set('user_id', uid);
		goto(`${base}/audit${params.toString() ? `?${params.toString()}` : ''}`, { keepFocus: true, noScroll: true });
	}

	function setOffset(offset: number) {
		const params = new URLSearchParams(page.url.searchParams);
		if (offset <= 0) params.delete('offset');
		else params.set('offset', String(offset));
		goto(`${base}/audit?${params.toString()}`, { keepFocus: true, noScroll: true });
	}

	async function exportEvents() {
		try {
			const params = currentParams();
			delete params.offset;
			delete params.limit;
			const rows = await exportJsonl.auditEvents({ ...params, limit: 1000 });
			toasts.success(`Exported ${rows} event${rows === 1 ? '' : 's'}.`);
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Export failed.');
		}
	}

	const columns: Column[] = [
		{ label: 'Time' },
		{ label: 'Event' },
		{ label: 'User' },
		{ label: 'Remote addr' },
		{ label: 'Detail' }
	];

	const totals = $derived(summary.data?.totals);

</script>

<svelte:head><title>Audit Log · llmtrace</title></svelte:head>

<PageHeader title="Audit Log" description="UI authentication and administrative events.">
	{#snippet actions()}
		<button type="button" class="btn" onclick={exportEvents}><Icon name="download" size={16} /> Export</button>
	{/snippet}
</PageHeader>

<div class="grid grid-cols-2 gap-3 md:grid-cols-4">
	<StatCard label="Events (7d)" value={formatNumber(totals?.event_count ?? 0)} icon="shield" loading={summary.loading} />
	<StatCard label="Distinct users" value={formatNumber(totals?.user_count ?? 0)} icon="key" loading={summary.loading} />
	<StatCard label="Remote addrs" value={formatNumber(totals?.remote_addr_count ?? 0)} icon="activity" loading={summary.loading} />
	<StatCard label="Event types" value={formatNumber(summary.data?.event_types.length ?? 0)} icon="list" loading={summary.loading} />
</div>

{#if (summary.data?.event_types.length ?? 0) > 0}
	<Card title="Event types (7d)" class="mt-4">
		<div class="flex flex-wrap gap-2">
			{#each summary.data?.event_types ?? [] as et (et.name)}
				<Badge tone="neutral">{et.name}: {formatNumber(et.event_count)}</Badge>
			{/each}
		</div>
	</Card>
{/if}

<form onsubmit={applyFilters} class="card mt-4 mb-4 flex flex-wrap items-end gap-3 p-3">
	<Field label="Event type" class="flex-1">
		<input class="input" type="text" name="event_type" placeholder="login_failed" value={page.url.searchParams.get('event_type') ?? ''} />
	</Field>
	<Field label="User ID" class="flex-1">
		<input class="input" type="text" name="user_id" placeholder="admin" value={page.url.searchParams.get('user_id') ?? ''} />
	</Field>
	<div class="flex gap-2">
		<button type="submit" class="btn btn-brand"><Icon name="filter" size={16} /> Apply</button>
		{#if page.url.search}
			<a class="btn" href={`${base}/audit`}><Icon name="x" size={16} /> Reset</a>
		{/if}
	</div>
</form>

<DataTable
	{columns}
	rows={events.data?.items ?? []}
	loading={events.loading}
	error={events.error}
	emptyTitle="No events"
	emptyMessage="No audit events matched your filters."
	onRetry={() => events.load()}
>
	{#snippet row(item: AuditEvent)}
		<td class="px-3 py-2 whitespace-nowrap">{formatDateTime(item.created_at)}</td>
		<td class="px-3 py-2"><Badge tone="info">{item.event_type}</Badge></td>
		<td class="px-3 py-2">{item.user_id ?? '—'}</td>
		<td class="px-3 py-2 font-mono text-xs">{item.remote_addr ?? '—'}</td>
		<td class="px-3 py-2 max-w-[24rem] truncate font-mono text-xs" title={JSON.stringify(item.detail)}>
			{Object.keys(item.detail).length ? JSON.stringify(item.detail) : '—'}
		</td>
	{/snippet}
</DataTable>

{#if events.data}
	<Pagination
		offset={events.data.page.offset}
		limit={events.data.page.limit}
		count={events.data.items.length}
		hasMore={events.data.page.has_more}
		onChange={setOffset}
	/>
{/if}
