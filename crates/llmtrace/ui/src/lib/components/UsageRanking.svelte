<script lang="ts">
	import { base } from '$app/paths';
	import { formatNumber } from '$lib/utils/format';
	import MiniBar from './MiniBar.svelte';

	interface Props {
		items: { name: string; request_count: number; error_count: number }[];
		dimension: 'model' | 'upstream_host';
		emptyMessage?: string;
	}
	let { items, dimension, emptyMessage = 'No traffic in this window.' }: Props = $props();
	const max = $derived(Math.max(1, ...items.map((item) => item.request_count)));
</script>

{#if items.length === 0}
	<p class="text-fg-muted text-sm">{emptyMessage}</p>
{:else}
	<div class="flex flex-col gap-3">
		{#each items as item (item.name)}
			<MiniBar label={item.name} value={item.request_count} {max}
				tone={dimension === 'upstream_host' ? 'var(--color-info)' : undefined}
				display={`${formatNumber(item.request_count)}${item.error_count > 0 ? ` · ${item.error_count} err` : ''}`}
				href={`${base}/requests?${dimension}=${encodeURIComponent(item.name)}`} />
		{/each}
	</div>
{/if}
