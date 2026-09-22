<script lang="ts">
	import type { ToolDefinition } from '$lib/transcript/tools';
	import CopyButton from './CopyButton.svelte';
	import Badge from './Badge.svelte';
	let { tool }: { tool: ToolDefinition } = $props();
	let expanded = $state(false);
</script>

<article class="min-w-0 rounded-lg border border-[var(--color-border)] p-3" aria-label={`Tool ${tool.name}`}>
	<div class="flex flex-wrap items-center gap-2">
		<h3 class="font-mono text-sm font-semibold [overflow-wrap:anywhere]">{tool.name}</h3>
		{#if tool.kind}<Badge>{tool.kind}</Badge>{/if}
	</div>
	{#if tool.description}
		<p class="mt-2 whitespace-pre-wrap text-sm [overflow-wrap:anywhere]">{tool.description}</p>
	{:else}
		<p class="text-fg-muted mt-2 text-xs">No description provided in this request.</p>
	{/if}
	<details class="mt-3 min-w-0 text-xs" bind:open={expanded}>
		<summary class="text-fg-muted hover:text-fg cursor-pointer">Parameters and full definition</summary>
		{#if expanded}
			<div class="mt-2 flex justify-end"><CopyButton text={tool.definition} label="Copy definition" /></div>
			<pre class="bg-surface-muted mt-2 min-w-0 rounded-lg p-3 whitespace-pre-wrap font-mono [overflow-wrap:anywhere]">{tool.definition}</pre>
		{/if}
	</details>
</article>
