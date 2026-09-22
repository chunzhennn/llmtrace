<script lang="ts">
	import { untrack } from 'svelte';
	let { label, content }: { label: string; content: string } = $props();
	let expanded = $state(untrack(() => content.length <= 8192));
</script>

<details class="mt-3 min-w-0 text-xs" bind:open={expanded}>
	<summary class="text-fg-muted hover:text-fg cursor-pointer font-medium">{label}</summary>
	{#if expanded}
		<pre class="bg-surface-muted mt-2 min-w-0 rounded-lg p-3 whitespace-pre-wrap font-mono [overflow-wrap:anywhere]">{content}</pre>
	{/if}
</details>
