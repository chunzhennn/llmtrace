<script lang="ts">
	import { untrack } from 'svelte';
	import { base } from '$app/paths';
	import type { ToolDefinition as Definition } from '$lib/transcript/tools';
	import ToolDefinition from './ToolDefinition.svelte';
	import Icon from './Icon.svelte';
	let { tools, initiallyOpen = true, requestId, repeated = false }: {
		tools: Definition[];
		initiallyOpen?: boolean;
		requestId?: string;
		repeated?: boolean;
	} = $props();
	let expanded = $state(untrack(() => initiallyOpen));
</script>

<details class="provided-tools min-w-0 rounded-lg border border-[var(--color-border)] p-3" bind:open={expanded}>
	<summary class="cursor-pointer text-sm">
		<span class="inline-flex items-center gap-1.5 align-middle font-medium"><Icon name="box" size={15} /> Tools available ({tools.length})</span>
		{#if repeated}<span class="text-fg-muted text-xs"> · Same definitions</span>{/if}
		{#if requestId}<span class="text-fg-muted text-xs"> · <a class="underline underline-offset-2" href={`${base}/requests/${requestId}`}>Request {requestId.slice(0, 8)}</a></span>{/if}
	</summary>
	{#if expanded}
		<div class="mt-3 flex min-w-0 flex-col gap-3">
			{#each tools as tool, index (index)}<ToolDefinition {tool} />{/each}
			{#if !tools.length}<p class="text-fg-muted text-sm">No tools provided in this request.</p>{/if}
		</div>
	{/if}
</details>
