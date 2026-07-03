<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		label: string;
		value?: string | number | null;
		mono?: boolean;
		children?: Snippet;
	}

	let { label, value, mono = false, children }: Props = $props();
</script>

<div class="flex flex-col gap-0.5 py-1.5">
	<span class="text-fg-muted text-xs font-medium uppercase tracking-wide">{label}</span>
	{#if children}
		<div class="text-sm">{@render children()}</div>
	{:else}
		<span class="text-sm break-all" class:font-mono={mono}>
			{value === null || value === undefined || value === '' ? '—' : value}
		</span>
	{/if}
</div>
