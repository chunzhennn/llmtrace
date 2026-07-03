<script lang="ts">
	import Icon from './Icon.svelte';

	interface Props {
		message?: string;
		onRetry?: () => void;
	}

	let { message = 'Something went wrong.', onRetry }: Props = $props();
</script>

<div class="flex flex-col items-center justify-center gap-3 px-4 py-10 text-center">
	<span style="color: var(--color-danger);"><Icon name="alert" size={28} /></span>
	<p class="max-w-md text-sm">{message}</p>
	{#if onRetry}
		<button type="button" class="btn" onclick={onRetry}>
			<Icon name="refresh" size={16} />
			Retry
		</button>
	{/if}
</div>
