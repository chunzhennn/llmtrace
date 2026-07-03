<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	interface Props {
		label: string;
		value: string | number;
		icon?: string;
		hint?: string;
		tone?: 'default' | 'success' | 'warning' | 'danger';
		loading?: boolean;
		footer?: Snippet;
	}

	let {
		label,
		value,
		icon,
		hint,
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

<div class="card p-4">
	<div class="flex items-start justify-between gap-2">
		<span class="text-fg-muted text-xs font-medium uppercase tracking-wide">{label}</span>
		{#if icon}
			<span style="color: {color};"><Icon name={icon} size={16} /></span>
		{/if}
	</div>
	{#if loading}
		<div class="mt-2 h-7 w-24 animate-pulse rounded" style="background-color: var(--color-surface-muted);"></div>
	{:else}
		<div class="mt-1 text-2xl font-semibold tabular-nums">{value}</div>
	{/if}
	{#if hint}
		<div class="text-fg-muted mt-1 text-xs">{hint}</div>
	{/if}
	{#if footer}
		<div class="mt-2">{@render footer()}</div>
	{/if}
</div>
