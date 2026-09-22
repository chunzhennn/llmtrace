<script lang="ts">
	import { base } from '$app/paths';
	import MessageBubble from './MessageBubble.svelte';
	import ProvidedTools from './ProvidedTools.svelte';
	import type { ToolSet } from '$lib/transcript/tools';
	import type { ContextNode, TranscriptBlock, TranscriptMessage } from '$lib/transcript/types';

	let { block, contextNodes, messagesById, toolSets }: {
		block: TranscriptBlock;
		toolSets: Map<number, ToolSet>;
		contextNodes: Map<number, ContextNode>;
		messagesById: Map<string, TranscriptMessage>;
	} = $props();
	let expanded = $state(false);
	const toolSet = $derived(block.toolSetId === null ? undefined : toolSets.get(block.toolSetId));
	const context = $derived.by(() => {
		if (!expanded) return [];
		const messages: TranscriptMessage[] = [];
		let id = block.reusedNode;
		while (id) {
			const node = contextNodes.get(id);
			if (!node) break;
			const message = messagesById.get(node.messageId);
			if (message) messages.push(message);
			id = node.parent;
		}
		return messages.reverse();
	});
</script>

<div class="flex min-w-0 flex-col gap-6" data-request-id={block.id}>
	{#if toolSet && (toolSet.tools.length || block.toolsChanged)}
		<ProvidedTools tools={toolSet.tools} initiallyOpen={block.toolsChanged} repeated={!block.toolsChanged} requestId={block.id} />
	{/if}
	{#if block.reusedCount}
		<details bind:open={expanded} class="text-fg-muted min-w-0 text-xs">
			<summary class="cursor-pointer py-1">
				{block.reusedCount} earlier message{block.reusedCount === 1 ? '' : 's'} reused
				· <a class="underline underline-offset-2" href={`${base}/requests/${block.id}`}>Request {block.id.slice(0, 8)}</a>
			</summary>
			{#if expanded}
				<div class="mt-4 flex min-w-0 flex-col gap-6 border-l-2 border-[var(--color-border)] pl-4" aria-label="Reused context">
					{#each context as message, index (index)}
						<MessageBubble role={message.role} label={message.label} content={message.content} requestId={block.id} createdAt={block.createdAt} fullContent />
					{/each}
				</div>
			{/if}
		</details>
	{/if}
	{#if block.notices.length}
		<div class="rounded-lg border border-[var(--color-warning)] p-3 text-xs" role="status">
			<a class="underline underline-offset-2" href={`${base}/requests/${block.id}`}>Request {block.id.slice(0, 8)}</a>
			{#each block.notices as notice}<p class="mt-1">{notice}</p>{/each}
		</div>
	{/if}
	{#each block.messages as message (message.id)}
		<MessageBubble role={message.role} label={message.label} content={message.content} requestId={block.id} createdAt={block.createdAt} fullContent />
	{/each}
	{#if !block.messages.length && !block.reusedCount && !block.notices.length}
		<p class="text-fg-muted text-xs"><a class="underline underline-offset-2" href={`${base}/requests/${block.id}`}>Request {block.id.slice(0, 8)}</a> · No conversation content in the captured bodies.</p>
	{/if}
</div>
