<script lang="ts">
	import { base } from '$app/paths';
	import { formatDateTime } from '$lib/utils/format';
	import Badge from './Badge.svelte';
	import TranscriptText from './TranscriptText.svelte';

	interface Props {
		role: string;
		content: string;
		createdAt?: string;
		requestId?: string;
		contentTruncated?: boolean;
		fullContent?: boolean;
		label?: string;
	}

	let { role, content, createdAt, requestId, contentTruncated = false, fullContent = false, label }: Props = $props();

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
	<div class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs" class:flex-row-reverse={isUser}>
		<span class="font-semibold uppercase tracking-wide" style="color: {color};">{role}</span>
		{#if label}<span class="text-fg-muted">{label}</span>{/if}
		{#if createdAt}<span class="text-fg-muted">{formatDateTime(createdAt)}</span>{/if}
		{#if requestId}
			<span class="inline-flex flex-wrap items-center gap-1.5" class:flex-row-reverse={isUser}>
				<a
					class="text-fg-muted hover:text-fg whitespace-nowrap underline underline-offset-2"
					href={`${base}/requests/${requestId}`}
					title={`View request ${requestId}`}
				>Request {requestId.slice(0, 8)}</a>
				{#if contentTruncated}
					<a href={`${base}/requests/${requestId}`} title="This message preview was shortened. Open the request to inspect the retained content.">
						<Badge tone="warning">Content shortened</Badge>
					</a>
				{/if}
			</span>
		{/if}
	</div>
	<div
		class="min-w-0 max-w-[85%] rounded-lg px-3 py-2 text-sm"
		style="border: 1px solid var(--color-border); background-color: {isUser
			? 'color-mix(in srgb, var(--color-info) 10%, transparent)'
			: 'var(--color-surface)'};"
	>
		{#if fullContent}
			<TranscriptText {content} />
		{:else}
			<pre class="whitespace-pre-wrap break-words font-sans">{content}{#if contentTruncated}<span aria-label="Preview shortened">…</span>{/if}</pre>
		{/if}
	</div>
</div>
