<script lang="ts">
	import { page } from '$app/state';
	import { listReturnHref } from '$lib/utils/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import MessageBubble from '$lib/components/MessageBubble.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Skeleton from '$lib/components/Skeleton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { copyText } from '$lib/utils/clipboard';
	import SessionRequests from '$lib/components/SessionRequests.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import * as sessionsApi from '$lib/api/endpoints/sessions';
	import { toasts } from '$lib/state/toast.svelte';
	import type { SessionDetail } from '$lib/api/types';
	import { formatNumber, formatDuration, formatBytes, formatCost, formatTokens } from '$lib/utils/format';

	const MESSAGE_PAGE = 50;

	const id = $derived(page.params.id ?? '');

	const detail = createResource<SessionDetail>((signal) =>
		sessionsApi.getSession(id, { messages_limit: MESSAGE_PAGE, messages_offset: 0 }, signal), () => id
	);
	const session = $derived(detail.data?.id === id ? detail.data : undefined);
	const employee = $derived(session?.user_name || session?.user_id);
	const messages = $derived(session?.messages ?? []);
	const messagesPage = $derived(session?.messages_page);
	const loading = $derived(detail.loading);
	const error = $derived(detail.error);
	const loadingMore = $derived(detail.loading && session !== undefined);

	async function loadSession() {
		await detail.load();
	}

	async function copySessionId() {
		try {
			await copyText(id);
			toasts.success('Session ID copied');
		} catch {
			toasts.error('Could not copy session ID');
		}
	}

	async function loadMoreMessages() {
		const current = session;
		if (detail.loading || !current?.messages_page.has_more) return;
		const result = await detail.load(async (signal) => {
			const next = await sessionsApi.getSession(current.id, {
				messages_limit: MESSAGE_PAGE,
				messages_offset: current.messages_page.next_offset
			}, signal);
			return { ...next, messages: [...current.messages, ...next.messages] };
		});
		if (!result && detail.data === current && detail.error) {
			toasts.error(detail.error);
			detail.error = null;
		}
	}


	const stats = $derived(session?.request_stats);
	const usageCoverage = $derived(`Usage reported for ${stats?.usage_known_count ?? 0} of ${stats?.request_count ?? 0} requests. Totals include available values only.`);
	const costCoverage = $derived(`Cost estimated for ${stats?.priced_request_count ?? 0} of ${stats?.request_count ?? 0} requests. Total includes available estimates only.`);
</script>

<svelte:head><title>Session · llmtrace</title></svelte:head>

<PageHeader title="Session detail">
	{#snippet description()}
		{#if employee}{employee} · {/if}Session
		<button
			type="button"
			class="hover:text-fg cursor-pointer font-mono underline underline-offset-2"
			onclick={copySessionId}
			aria-label="Copy full session ID"
			title={`Copy full session ID: ${id}`}
		>{id.slice(0, 8)}</button>
	{/snippet}
	{#snippet actions()}
		<a class="btn" href={listReturnHref(page.url.searchParams.get('from'), `${base}/sessions`)}><Icon name="chevron-left" size={16} /> Back</a>
		<a class="btn" href={`${base}/sessions/${id}/transcript${page.url.search}`}><Icon name="messages" size={16} /> View transcript</a>
		{#if session}
			<a class="btn" href={sessionsApi.sessionExportUrl(id)} download>
				<Icon name="download" size={16} /> Export full session
			</a>
		{/if}
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
		<StatCard label="Avg duration" value={formatDuration(stats?.avg_duration_ms)} icon="activity" />
		<StatCard label="Max duration" value={formatDuration(stats?.max_duration_ms)} icon="clock" />
		<StatCard label="Avg TTFT" value={formatDuration(stats?.avg_ttft_ms)} icon="activity" />
	</div>

	<div class="mt-4 grid grid-cols-2 gap-3 md:grid-cols-4">
		<StatCard label="Input tokens" value={formatTokens(stats?.input_tokens)} icon="activity" tooltip={usageCoverage} />
		<StatCard label="Output tokens" value={formatTokens(stats?.output_tokens)} icon="activity" tooltip={usageCoverage} />
		<StatCard label="Tool calls captured" value={formatNumber(stats?.tool_call_count ?? 0)} icon="box" />
		<StatCard label="Estimated token cost" value={formatCost(stats?.estimated_cost_microusd)} icon="database" tooltip={costCoverage} />
	</div>
	<div id="transcript" class="mt-4 grid scroll-mt-20 grid-cols-1 gap-4 xl:grid-cols-2">
		<Card title="Transcript previews" subtitle={`${messages.length} messages loaded · request snapshots may repeat conversation history`}>
			{#if messages.length === 0}
				<EmptyState icon="messages" title="No messages" message="No parsed messages for this session." />
			{:else}
				<div class="flex max-h-[36rem] flex-col gap-4 overflow-y-auto pr-1">
					{#each messages as message (message.id)}
						<MessageBubble
							role={message.role}
							content={message.content}
							createdAt={message.created_at}
							requestId={message.request_id}
							contentTruncated={message.content_truncated === true}
						/>
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

	<details class="card mt-4 p-4 text-sm">
		<summary class="cursor-pointer font-medium">Session identifiers</summary>
		<p class="text-fg-muted mt-3">Session key</p>
		<p class="mt-1 break-all font-mono text-xs">{session.session_key}</p>
		<p class="text-fg-muted mt-3">Session ID</p>
		<p class="mt-1 break-all font-mono text-xs">{id}</p>
	</details>

	{#if session.summary && Object.keys(session.summary).length > 0}
		<Card title="Session summary" class="mt-4" bodyClass="p-4">
			<pre class="overflow-auto rounded-lg p-3 text-xs" style="background-color: var(--color-surface-muted); max-height: 20rem;">{JSON.stringify(session.summary, null, 2)}</pre>
		</Card>
	{/if}
{/if}
