<script lang="ts">
	import { fly } from 'svelte/transition';
	import { toasts } from '$lib/state/toast.svelte';

	const toneClass: Record<string, string> = {
		success: 'border-l-[var(--color-success)]',
		error: 'border-l-[var(--color-danger)]',
		info: 'border-l-[var(--color-info)]'
	};
</script>

<div class="pointer-events-none fixed bottom-4 right-4 z-50 flex w-full max-w-sm flex-col gap-2">
	{#each toasts.items as toast (toast.id)}
		<div
			class="card pointer-events-auto flex items-start gap-3 border-l-4 p-3 shadow-lg {toneClass[
				toast.kind
			] ?? ''}"
			transition:fly={{ y: 12, duration: 180 }}
			role="status"
		>
			<span class="flex-1 text-sm break-words">{toast.message}</span>
			<button
				type="button"
				class="text-fg-muted hover:text-fg text-lg leading-none"
				aria-label="Dismiss notification"
				onclick={() => toasts.dismiss(toast.id)}
			>
				×
			</button>
		</div>
	{/each}
</div>
