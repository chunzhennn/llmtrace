<script lang="ts">
	import Icon from './Icon.svelte';
	import { formatNumber } from '$lib/utils/format';

	interface Props {
		offset: number;
		limit: number;
		count: number;
		hasMore: boolean;
		onChange: (offset: number) => void;
	}

	let { offset, limit, count, hasMore, onChange }: Props = $props();

	const from = $derived(count === 0 ? 0 : offset + 1);
	const to = $derived(offset + count);
</script>

<div class="flex items-center justify-between gap-3 px-1 py-2 text-sm">
	<span class="text-fg-muted">
		{#if count === 0}
			No results
		{:else}
			Showing {formatNumber(from)}–{formatNumber(to)}
		{/if}
	</span>
	<div class="flex items-center gap-2">
		<button
			type="button"
			class="btn !px-2 !py-1"
			disabled={offset <= 0}
			onclick={() => onChange(Math.max(0, offset - limit))}
			aria-label="Previous page"
		>
			<Icon name="chevron-left" size={16} />
			Prev
		</button>
		<button
			type="button"
			class="btn !px-2 !py-1"
			disabled={!hasMore}
			onclick={() => onChange(offset + limit)}
			aria-label="Next page"
		>
			Next
			<Icon name="chevron-right" size={16} />
		</button>
	</div>
</div>
