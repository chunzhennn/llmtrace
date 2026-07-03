<script lang="ts">
	import { base } from '$app/paths';
	import DataTable, { type Column } from './DataTable.svelte';
	import Pagination from './Pagination.svelte';
	import StatusPill from './StatusPill.svelte';
	import Icon from './Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as sessionsApi from '$lib/api/endpoints/sessions';
	import { exportJsonl } from '$lib/api/download';
	import { toasts } from '$lib/state/toast.svelte';
	import type { SessionRequestsResponse, RequestSummary } from '$lib/api/types';
	import { formatDateTime, formatDuration } from '$lib/utils/format';

	const REQUEST_PAGE = 25;

	interface Props {
		id: string;
	}

	let { id }: Props = $props();

	let offset = $state(0);
	const requests = new Resource<SessionRequestsResponse>((signal) =>
		sessionsApi.listSessionRequests(id, { limit: REQUEST_PAGE, offset }, signal)
	);

	$effect(() => {
		void offset;
		requests.load();
	});

	async function exportRequests() {
		try {
			const rows = await exportJsonl.sessionRequests(id, { limit: 500 });
			toasts.success(`Exported ${rows} request${rows === 1 ? '' : 's'}.`);
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Export failed.');
		}
	}

	const columns: Column[] = [
		{ label: 'Started' },
		{ label: 'Status' },
		{ label: 'Model' },
		{ label: 'Duration', align: 'right' }
	];
</script>

<div class="mb-2 flex items-center justify-between">
	<h2 class="text-sm font-semibold">Requests</h2>
	<button type="button" class="btn !px-2 !py-1 text-xs" onclick={exportRequests}>
		<Icon name="download" size={14} /> Export
	</button>
</div>
<DataTable
	{columns}
	rows={requests.data?.items ?? []}
	loading={requests.loading}
	error={requests.error}
	emptyTitle="No requests"
	onRetry={() => requests.load()}
>
	{#snippet row(item: RequestSummary)}
		<td class="px-3 py-2 whitespace-nowrap">
			<a class="hover:underline" style="color: var(--color-brand);" href={`${base}/requests/${item.id}`}>
				{formatDateTime(item.started_at)}
			</a>
		</td>
		<td class="px-3 py-2"><StatusPill status={item.status} error={item.error} /></td>
		<td class="px-3 py-2">{item.model ?? '—'}</td>
		<td class="px-3 py-2 text-right tabular-nums">{formatDuration(item.duration_ms)}</td>
	{/snippet}
</DataTable>
{#if requests.data}
	<Pagination
		offset={requests.data.page.offset}
		limit={requests.data.page.limit}
		count={requests.data.items.length}
		hasMore={requests.data.page.has_more}
		onChange={(next) => (offset = next)}
	/>
{/if}
