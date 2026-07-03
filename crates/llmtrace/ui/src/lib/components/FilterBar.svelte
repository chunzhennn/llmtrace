<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		actions?: Snippet;
	}

	let { children, actions }: Props = $props();
</script>

<div class="card flex flex-wrap items-end gap-3 p-3">
	{@render children()}
	{#if actions}
		<div class="ml-auto flex flex-wrap items-end gap-2">{@render actions()}</div>
	{/if}
</div>
