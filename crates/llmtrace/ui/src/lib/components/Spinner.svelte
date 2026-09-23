<script lang="ts">
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';

	interface Props {
		size?: number;
		label?: string;
	}

	let { size = 18, label }: Props = $props();
</script>

<span class="text-fg-muted inline-flex items-center gap-2 text-sm">
	<RefreshCwIcon {size} class="animate-spin" />
	{#if label}{label}{/if}
</span>
