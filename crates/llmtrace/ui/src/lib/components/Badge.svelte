<script lang="ts" module>
	export type BadgeTone = 'success' | 'info' | 'warning' | 'danger' | 'neutral' | 'brand';
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		tone?: BadgeTone;
		class?: string;
		children: Snippet;
	}

	let { tone = 'neutral', class: className = '', children }: Props = $props();

	const toneVar: Record<BadgeTone, string> = {
		success: 'var(--color-success)',
		info: 'var(--color-info)',
		warning: 'var(--color-warning)',
		danger: 'var(--color-danger)',
		brand: 'var(--color-brand)',
		neutral: 'var(--color-fg-muted)'
	};

	const color = $derived(toneVar[tone]);
</script>

<span
	class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium {className}"
	style="color: {color}; background-color: color-mix(in srgb, {color} 14%, transparent);"
>
	{@render children()}
</span>
