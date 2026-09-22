<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	interface Props {
		label: string;
		value: string | number;
		icon?: string;
		hint?: string;
		tooltip?: string;
		tone?: 'default' | 'success' | 'warning' | 'danger';
		loading?: boolean;
		footer?: Snippet;
	}

	let {
		label,
		value,
		icon,
		hint,
		tooltip,
		tone = 'default',
		loading = false,
		footer
	}: Props = $props();

	const toneColor: Record<string, string> = {
		default: 'var(--color-brand)',
		success: 'var(--color-success)',
		warning: 'var(--color-warning)',
		danger: 'var(--color-danger)'
	};
	const color = $derived(toneColor[tone]);
</script>

<div class="card relative min-w-0 p-3 sm:p-4">
	<div class="flex items-start justify-between gap-2">
		<span class="text-fg-muted text-xs font-medium uppercase tracking-wide">{label}</span>
		{#if icon}
			<span style="color: {color};"><Icon name={icon} size={16} /></span>
		{/if}
	</div>
	{#if loading}
		<div class="mt-2 h-7 w-24 animate-pulse rounded" style="background-color: var(--color-surface-muted);"></div>
	{:else}
		<div class="mt-1 flex items-center gap-1">
			<div class="min-w-0 text-xl font-semibold tabular-nums [overflow-wrap:anywhere] sm:text-2xl">{value}</div>
			{#if tooltip}
				<details class="shrink-0">
					<summary class="text-fg-muted hover:text-fg flex size-7 cursor-pointer list-none items-center justify-center rounded focus-visible:outline-2 focus-visible:outline-offset-2 [&::-webkit-details-marker]:hidden" aria-label={`About ${label.toLowerCase()}`} title={tooltip}>
						<Icon name="info" size={14} />
					</summary>
					<p class="bg-surface text-fg absolute inset-x-0 top-full z-20 mt-1 rounded-lg border border-[var(--color-border)] p-3 text-xs shadow-lg">{tooltip}</p>
				</details>
			{/if}
		</div>
	{/if}
	{#if hint}
		<div class="text-fg-muted mt-1 text-xs">{hint}</div>
	{/if}
	{#if footer}
		<div class="mt-2">{@render footer()}</div>
	{/if}
</div>
