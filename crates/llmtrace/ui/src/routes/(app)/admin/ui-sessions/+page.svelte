<script lang="ts">
	import { onMount } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as admin from '$lib/api/endpoints/admin';
	import { toasts } from '$lib/state/toast.svelte';
	import type { UiSessionsResponse, UiSession } from '$lib/api/types';
	import { formatDateTime, formatSecondsDuration, truncateMiddle } from '$lib/utils/format';

	const LIMIT = 50;
	let includeExpired = $state(false);
	let offset = $state(0);
	let revoking = $state<string | null>(null);

	const sessions = new Resource<UiSessionsResponse>((signal) =>
		admin.listUiSessions({ include_expired: includeExpired, limit: LIMIT, offset }, signal)
	);

	$effect(() => {
		void includeExpired;
		void offset;
		sessions.load();
	});

	onMount(() => sessions.load());

	async function revoke(item: UiSession) {
		if (!confirm(`Revoke session for ${item.display_name} (${item.user_id})?`)) return;
		revoking = item.session_hash;
		try {
			await admin.revokeUiSession(item.session_hash);
			toasts.success('Session revoked.');
			await sessions.load();
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Revoke failed.');
		} finally {
			revoking = null;
		}
	}

	const columns: Column[] = [
		{ label: 'User' },
		{ label: 'Login' },
		{ label: 'Created' },
		{ label: 'Expires' },
		{ label: 'Status' },
		{ label: '', align: 'right' }
	];
</script>

<svelte:head><title>UI Sessions · llmtrace</title></svelte:head>

<PageHeader title="UI Sessions" description="Active dashboard sessions. Revoke to force re-authentication.">
	{#snippet actions()}
		<label class="flex cursor-pointer items-center gap-2 text-sm">
			<input type="checkbox" bind:checked={includeExpired} onchange={() => (offset = 0)} />
			Include expired
		</label>
	{/snippet}
</PageHeader>

<DataTable
	{columns}
	rows={sessions.data?.items ?? []}
	loading={sessions.loading}
	error={sessions.error}
	emptyTitle="No sessions"
	emptyMessage="There are no active UI sessions."
	onRetry={() => sessions.load()}
>
	{#snippet row(item: UiSession)}
		<td class="px-3 py-2">
			<div class="font-medium">{item.display_name}</div>
			<div class="text-fg-muted font-mono text-xs">{truncateMiddle(item.user_id, 28)}</div>
		</td>
		<td class="px-3 py-2"><Badge tone="neutral">{item.login_method}</Badge></td>
		<td class="px-3 py-2 whitespace-nowrap">{formatDateTime(item.created_at)}</td>
		<td class="px-3 py-2 whitespace-nowrap">{formatDateTime(item.expires_at)}</td>
		<td class="px-3 py-2">
			{#if item.expired}
				<Badge tone="danger">expired</Badge>
			{:else}
				<Badge tone="success">{formatSecondsDuration(item.expires_in_secs)} left</Badge>
			{/if}
		</td>
		<td class="px-3 py-2 text-right">
			<button
				type="button"
				class="btn !px-2 !py-1 text-xs"
				disabled={revoking === item.session_hash}
				onclick={() => revoke(item)}
			>
				{#if revoking === item.session_hash}<Icon name="refresh" size={14} class="animate-spin" />{:else}<Icon name="trash" size={14} />{/if}
				Revoke
			</button>
		</td>
	{/snippet}
</DataTable>

{#if sessions.data}
	<Pagination
		offset={sessions.data.page.offset}
		limit={sessions.data.page.limit}
		count={sessions.data.items.length}
		hasMore={sessions.data.page.has_more}
		onChange={(next) => (offset = next)}
	/>
{/if}
