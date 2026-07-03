<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	interface Props {
		icon?: string;
		title?: string;
		message?: string;
		children?: Snippet;
	}

	let {
		icon = 'inbox',
		title = 'Nothing here yet',
		message,
		children
	}: Props = $props();
</script>

<div class="flex flex-col items-center justify-center gap-2 px-4 py-10 text-center">
	<span class="text-fg-muted"><Icon name={icon} size={28} /></span>
	<p class="text-sm font-medium">{title}</p>
	{#if message}<p class="text-fg-muted max-w-sm text-xs">{message}</p>{/if}
	{#if children}<div class="mt-2">{@render children()}</div>{/if}
</div>
