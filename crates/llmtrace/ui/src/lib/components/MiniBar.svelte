<script lang="ts">
	interface Props {
		label: string;
		value: number;
		max: number;
		display?: string;
		tone?: string;
		href?: string;
	}

	let { label, value, max, display, tone = 'var(--color-brand)', href }: Props = $props();

	const pct = $derived(max > 0 ? Math.max(2, Math.round((value / max) * 100)) : 0);
</script>

<div class="flex flex-col gap-1">
	<div class="flex items-center justify-between gap-2 text-sm">
		{#if href}
			<a class="truncate hover:underline" style="color: var(--color-brand);" {href}>{label}</a>
		{:else}
			<span class="truncate" title={label}>{label}</span>
		{/if}
		<span class="text-fg-muted tabular-nums">{display ?? value}</span>
	</div>
	<div class="h-1.5 w-full overflow-hidden rounded-full" style="background-color: var(--color-surface-muted);">
		<div class="h-full rounded-full" style="width: {pct}%; background-color: {tone};"></div>
	</div>
</div>
