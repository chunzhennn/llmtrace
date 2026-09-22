<script lang="ts">
	import { page } from '$app/state';
	import { base } from '$app/paths';
	import { onDestroy } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import TranscriptRequest from '$lib/components/TranscriptRequest.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Skeleton from '$lib/components/Skeleton.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { copyText } from '$lib/utils/clipboard';
	import { formatNumber } from '$lib/utils/format';
	import { toasts } from '$lib/state/toast.svelte';
	import { sessionExportUrl } from '$lib/api/endpoints/sessions';
	import { streamSessionTranscript } from '$lib/transcript/client';
	import type { ContextNode, ExportHeader, TranscriptBlock, TranscriptMessage } from '$lib/transcript/types';
	import type { ToolSet } from '$lib/transcript/tools';

	const id = $derived(page.params.id ?? '');
	const sessionHref = $derived(`${base}/sessions/${id}${page.url.search}`);
	let header = $state<ExportHeader>();
	let blocks = $state<TranscriptBlock[]>([]);
	// Only append new nodes and messages; repeated context stores a single trie-node reference.
	let contextNodes = $state.raw(new Map<number, ContextNode>());
	let messagesById = $state.raw(new Map<string, TranscriptMessage>());
	let toolSets = $state.raw(new Map<number, ToolSet>());
	let messageCount = $state(0);
	let reusedCount = $state(0);
	let loading = $state(true);
	let complete = $state(false);
	let error = $state<string | null>(null);
	let controller: AbortController | undefined;
	const employee = $derived(header?.session.user_name || header?.session.user_id);

	async function load(sessionId = id) {
		controller?.abort();
		const current = new AbortController(); controller = current;
		loading = true; complete = false; error = null;
		try {
			await streamSessionTranscript(sessionId, current.signal, (event) => {
				if (event.type === 'header') {
					header = event.header; blocks = []; messageCount = 0; reusedCount = 0;
					contextNodes = new Map(); messagesById = new Map(); toolSets = new Map();
				} else if (event.type === 'request') {
					if (event.toolSet) toolSets.set(event.toolSet.id, event.toolSet);
					for (const node of event.nodes) contextNodes.set(node.id, node);
					for (const message of event.block.messages) messagesById.set(message.id, message);
					blocks.push(event.block);
					messageCount += event.block.messages.length;
					reusedCount += event.block.reusedCount;
				} else if (event.type === 'end') complete = true;
			});
		} catch (reason) {
			if (!current.signal.aborted) error = reason instanceof Error ? reason.message : 'Could not load transcript.';
		} finally {
			if (!current.signal.aborted) loading = false;
		}
	}
	$effect(() => {
		const sessionId = id;
		header = undefined; blocks = []; messageCount = 0; reusedCount = 0;
		void load(sessionId);
	});
	onDestroy(() => controller?.abort());

	async function copySessionId() {
		try { await copyText(id); toasts.success('Session ID copied'); }
		catch { toasts.error('Could not copy session ID'); }
	}
</script>

<svelte:head><title>Transcript · llmtrace</title></svelte:head>

<PageHeader title="Transcript">
	{#snippet description()}
		{#if employee}{employee} · {/if}Session
		<button type="button" class="hover:text-fg cursor-pointer font-mono underline underline-offset-2" onclick={copySessionId}
			aria-label="Copy full session ID" title={`Copy full session ID: ${id}`}>{id.slice(0, 8)}</button>
	{/snippet}
	{#snippet actions()}
		<a class="btn" href={sessionHref}><Icon name="chevron-left" size={16} /> Back to session</a>
		{#if header}<a class="btn" href={sessionExportUrl(id)} download><Icon name="download" size={16} /> Export full session</a>{/if}
	{/snippet}
</PageHeader>

{#if loading && !header}
	<div class="flex flex-col gap-4"><Skeleton height="12rem" /><Skeleton height="12rem" /></div>
{:else if error && !header}
	<Card><ErrorState message={error} onRetry={() => load()} /></Card>
{:else if header}
	<Card title="Conversation" bodyClass="p-4 md:p-6"
		subtitle={`${formatNumber(messageCount)} messages · ${formatNumber(blocks.length)} of ${formatNumber(header.request_count)} requests · ${formatNumber(reusedCount)} repeated context messages folded`}>
		{#if !blocks.length && complete}
			<EmptyState icon="messages" title="No messages" message="This session has no captured requests." />
		{:else}
			<ol class="flex min-w-0 flex-col gap-6" aria-label="Conversation requests">
				{#each blocks as block (block.id)}
					<li class="min-w-0"><TranscriptRequest {block} {contextNodes} {messagesById} {toolSets} /></li>
				{/each}
			</ol>
		{/if}
		{#if loading}
			<div class="mt-6" role="status"><Spinner label="Loading full transcript…" /></div>
		{:else if error}
			<div class="mt-6"><ErrorState message={error} onRetry={() => load()} /></div>
		{:else if complete}
			<p class="text-fg-muted mt-6 text-center text-xs" role="status">All captured requests loaded.</p>
		{/if}
	</Card>
{/if}
