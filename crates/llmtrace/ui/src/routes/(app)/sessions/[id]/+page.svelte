<script lang="ts">
	import { page } from '$app/state';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import MessageBubble from '$lib/components/MessageBubble.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Skeleton from '$lib/components/Skeleton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import CopyButton from '$lib/components/CopyButton.svelte';
	import SessionRequests from '$lib/components/SessionRequests.svelte';
	import { ApiError } from '$lib/api/client';
	import * as sessionsApi from '$lib/api/endpoints/sessions';
	import { exportJsonl } from '$lib/api/download';
	import { toasts } from '$lib/state/toast.svelte';
	import type { SessionDetail, SessionMessage, Page } from '$lib/api/types';
	import { formatNumber, formatMs, formatBytes } from '$lib/utils/format';

	const MESSAGE_PAGE = 50;

	const id = $derived(page.params.id ?? '');

	let session = $state<SessionDetail | undefined>();
	let loading = $state(true);
	let error = $state<string | null>(null);
	let messages = $state<SessionMessage[]>([]);
	let messagesPage = $state<Page | undefined>();
	let loadingMore = $state(false);

	async function loadSession() {
		loading = true;
		error = null;
		try {
			const data = await sessionsApi.getSession(id, {
				messages_limit: MESSAGE_PAGE,
				messages_offset: 0
			});
			session = data;
			messages = data.messages;
			messagesPage = data.messages_page;
		} catch (err) {
			error = err instanceof ApiError || err instanceof Error ? err.message : 'Failed to load session';
		} finally {
			loading = false;
		}
	}

	async function loadMoreMessages() {
		if (loadingMore) return;
		loadingMore = true;
		try {
			const data = await sessionsApi.getSession(id, {
				messages_limit: MESSAGE_PAGE,
				messages_offset: messages.length
			});
			messages = [...messages, ...data.messages];
			messagesPage = data.messages_page;
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Failed to load more messages');
		} finally {
			loadingMore = false;
		}
	}

	$effect(() => {
		void id;
		loadSession();
	});

	async function exportMessages() {
		try {
			const rows = await exportJsonl.sessionMessages(id, { messages_limit: 1000 });
			toasts.success(`Exported ${rows} message${rows === 1 ? '' : 's'}.`);
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Export failed.');
		}
	}

	const stats = $derived(session?.request_stats);
</script>

<svelte:head><title>Session · llmtrace</title></svelte:head>

<PageHeader title="Session detail" description={session?.session_key ?? id}>
	{#snippet actions()}
		<a class="btn" href={`${base}/sessions`}><Icon name="chevron-left" size={16} /> Back</a>
		<CopyButton text={id} label="Copy ID" />
	{/snippet}
</PageHeader>

{#if loading && !session}
	<div class="flex flex-col gap-3"><Skeleton height="6rem" /><Skeleton height="16rem" /></div>
{:else if error}
	<Card><ErrorState message={error} onRetry={loadSession} /></Card>
{:else if session}
	<div class="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
		<StatCard label="Requests" value={formatNumber(stats?.request_count ?? 0)} icon="list" />
		<StatCard label="Errors" value={formatNumber(stats?.error_count ?? 0)} icon="alert" tone={stats && stats.error_count > 0 ? 'danger' : 'default'} />
		<StatCard label="Captured" value={formatBytes(stats?.captured_bytes ?? 0)} icon="database" />
		<StatCard label="Avg duration" value={formatMs(stats?.avg_duration_ms)} icon="activity" />
		<StatCard label="Max duration" value={formatMs(stats?.max_duration_ms)} icon="clock" />
		<StatCard label="Avg TTFT" value={formatMs(stats?.avg_ttft_ms)} icon="activity" />
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 xl:grid-cols-2">
		<Card title="Transcript" subtitle={`${messages.length} message${messages.length === 1 ? '' : 's'} loaded`}>
			{#snippet actions()}
				<button type="button" class="btn !px-2 !py-1 text-xs" onclick={exportMessages}>
					<Icon name="download" size={14} /> Export
				</button>
			{/snippet}
			{#if messages.length === 0}
				<EmptyState icon="messages" title="No messages" message="No parsed messages for this session." />
			{:else}
				<div class="flex max-h-[36rem] flex-col gap-4 overflow-y-auto pr-1">
					{#each messages as message (message.id)}
						<MessageBubble role={message.role} content={message.content} createdAt={message.created_at} />
					{/each}
				</div>
				{#if messagesPage?.has_more}
					<div class="mt-3 flex justify-center">
						<button type="button" class="btn" onclick={loadMoreMessages} disabled={loadingMore}>
							{#if loadingMore}<Icon name="refresh" size={16} class="animate-spin" /> Loading…{:else}Load more{/if}
						</button>
					</div>
				{/if}
			{/if}
		</Card>

		<div>
			{#key id}
				<SessionRequests {id} />
			{/key}
		</div>
	</div>

	{#if session.summary && Object.keys(session.summary).length > 0}
		<Card title="Session summary" class="mt-4" bodyClass="p-4">
			<pre class="overflow-auto rounded-lg p-3 text-xs" style="background-color: var(--color-surface-muted); max-height: 20rem;">{JSON.stringify(session.summary, null, 2)}</pre>
		</Card>
	{/if}
{/if}
