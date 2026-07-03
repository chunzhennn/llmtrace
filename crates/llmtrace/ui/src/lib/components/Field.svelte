<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		label: string;
		for?: string;
		hint?: string;
		class?: string;
		children: Snippet;
	}

	let { label, for: forId, hint, class: className = '', children }: Props = $props();
</script>

<div class="flex flex-col {className}">
	<label class="label" for={forId}>{label}</label>
	{@render children()}
	{#if hint}<span class="text-fg-muted mt-1 text-xs">{hint}</span>{/if}
</div>
