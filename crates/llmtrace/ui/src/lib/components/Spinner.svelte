<script lang="ts">
	import Icon from './Icon.svelte';

	interface Props {
		size?: number;
		label?: string;
	}

	let { size = 18, label }: Props = $props();
</script>

<span class="text-fg-muted inline-flex items-center gap-2 text-sm">
	<Icon name="refresh" {size} class="animate-spin" />
	{#if label}{label}{/if}
</span>
