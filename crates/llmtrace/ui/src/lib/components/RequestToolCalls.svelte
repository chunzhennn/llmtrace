<script lang="ts">
	import Card from './Card.svelte';
	import Badge from './Badge.svelte';
	import EmptyState from './EmptyState.svelte';
	import ToolCallContent from './ToolCallContent.svelte';
	import type { RequestToolHistory } from '$lib/transcript/request-tools';
	let { history, notice }: { history: RequestToolHistory; notice?: string | null } = $props();
	const resultCount = $derived(history.calls.reduce((count, call) => count + call.results.length, history.unmatchedResults.length));
</script>

<Card title="Tool calls in request history" subtitle={`${history.calls.length} ${history.calls.length === 1 ? 'call' : 'calls'} · ${resultCount} ${resultCount === 1 ? 'result' : 'results'} sent in this request`}>
	{#if notice}<p class="text-fg-muted mb-3 text-sm" role="status">{notice}</p>{/if}
	<div class="flex min-w-0 flex-col gap-4">
		{#each history.calls as call, index (index)}
			<article class="min-w-0 rounded-lg border border-[var(--color-border)] p-3" aria-label={`Request tool call ${call.name}`}>
				<div class="flex flex-wrap items-center gap-2">
					<h3 class="font-mono text-sm font-semibold [overflow-wrap:anywhere]">{call.name}</h3>
					{#if call.id}<span class="text-fg-muted font-mono text-xs [overflow-wrap:anywhere]">{call.id}</span>{/if}
				</div>
				<ToolCallContent label="Arguments" content={call.arguments} />
				{#each call.results as result, index (index)}
					{#if result.isError}<div class="mt-3"><Badge tone="danger">Tool error</Badge></div>{/if}
					<ToolCallContent label={call.results.length > 1 ? `Result ${index + 1}` : 'Result'} content={result.content} />
				{/each}
				{#if !call.results.length}<p class="text-fg-muted mt-3 text-xs">No matching result in this request.</p>{/if}
			</article>
		{/each}
		{#each history.unmatchedResults as result, index (index)}
			<article class="min-w-0 rounded-lg border border-[var(--color-border)] p-3" aria-label="Tool result without a call in this request">
				<p class="text-sm font-medium">Tool result {#if result.callId}<span class="text-fg-muted font-mono text-xs [overflow-wrap:anywhere]">{result.callId}</span>{/if}</p>
				<p class="text-fg-muted mt-1 text-xs">The corresponding call is not included in this request.</p>
				{#if result.isError}<div class="mt-3"><Badge tone="danger">Tool error</Badge></div>{/if}
				<ToolCallContent label="Result" content={result.content} />
			</article>
		{/each}
		{#if !history.calls.length && !resultCount && !notice}
			<EmptyState icon="box" title="No tool calls in request history" message="This request body contains no tool calls or results." />
		{/if}
	</div>
</Card>
