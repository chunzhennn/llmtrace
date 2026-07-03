<script lang="ts">
	import { formatDateTime } from '$lib/utils/format';

	interface Props {
		role: string;
		content: string;
		createdAt?: string;
	}

	let { role, content, createdAt }: Props = $props();

	const normalizedRole = $derived(role.toLowerCase());

	const roleColor: Record<string, string> = {
		user: 'var(--color-info)',
		assistant: 'var(--color-brand)',
		system: 'var(--color-fg-muted)',
		tool: 'var(--color-warning)'
	};
	const color = $derived(roleColor[normalizedRole] ?? 'var(--color-fg-muted)');
	const isUser = $derived(normalizedRole === 'user');
</script>

<div class="flex flex-col gap-1" class:items-end={isUser}>
	<div class="flex items-center gap-2 text-xs">
		<span class="font-semibold uppercase tracking-wide" style="color: {color};">{role}</span>
		{#if createdAt}<span class="text-fg-muted">{formatDateTime(createdAt)}</span>{/if}
	</div>
	<div
		class="max-w-[85%] rounded-lg px-3 py-2 text-sm"
		style="border: 1px solid var(--color-border); background-color: {isUser
			? 'color-mix(in srgb, var(--color-info) 10%, transparent)'
			: 'var(--color-surface)'};"
	>
		<pre class="whitespace-pre-wrap break-words font-sans">{content}</pre>
	</div>
</div>
