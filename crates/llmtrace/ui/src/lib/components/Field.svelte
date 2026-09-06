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

<label class="flex min-w-0 flex-col {className}" for={forId}>
	<span class="label">{label}</span>
	{@render children()}
	{#if hint}<span class="text-fg-muted mt-1 text-xs">{hint}</span>{/if}
</label>
