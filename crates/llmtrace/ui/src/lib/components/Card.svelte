<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		title?: string;
		subtitle?: string;
		class?: string;
		bodyClass?: string;
		actions?: Snippet;
		children: Snippet;
	}

	let {
		title,
		subtitle,
		class: className = '',
		bodyClass = 'p-4',
		actions,
		children
	}: Props = $props();
</script>

<section class="card {className}">
	{#if title || actions}
		<div
			class="flex items-center justify-between gap-3 px-4 py-3"
			style="border-bottom: 1px solid var(--color-border);"
		>
			<div class="min-w-0">
				{#if title}<h2 class="truncate text-sm font-semibold">{title}</h2>{/if}
				{#if subtitle}<p class="text-fg-muted truncate text-xs">{subtitle}</p>{/if}
			</div>
			{#if actions}
				<div class="flex shrink-0 items-center gap-2">{@render actions()}</div>
			{/if}
		</div>
	{/if}
	<div class={bodyClass}>
		{@render children()}
	</div>
</section>
