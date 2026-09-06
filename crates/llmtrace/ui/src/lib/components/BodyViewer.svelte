<script lang="ts">
	import Badge from './Badge.svelte';
	import CopyButton from './CopyButton.svelte';
	import Icon from './Icon.svelte';
	import JsonViewer from './JsonViewer.svelte';
	import EmptyState from './EmptyState.svelte';
	import { formatBytes, safeJsonParse } from '$lib/utils/format';

	interface Props {
		body: string;
		contentType?: string | null;
		byteSize?: number | null;
		truncated?: boolean;
		filename?: string;
	}

	let {
		body,
		contentType,
		byteSize,
		truncated = false,
		filename = 'body.txt'
	}: Props = $props();

	let mode = $state<'auto' | 'raw'>('auto');
	const PREVIEW_CHARS = 128 * 1024;
	const large = $derived(body.length > PREVIEW_CHARS);
	const preview = $derived(body.slice(0, PREVIEW_CHARS));

	const parsed = $derived.by(() => {
		if (large) return { ok: false, value: null };
		const trimmed = body.trim();
		if (trimmed === '') return { ok: false, value: null };
		const looksJson =
			(contentType?.includes('json') ?? false) ||
			trimmed.startsWith('{') ||
			trimmed.startsWith('[');
		if (!looksJson) return { ok: false, value: null };
		return safeJsonParse(trimmed);
	});

	const showJson = $derived(mode === 'auto' && parsed.ok);

	function download() {
		const blob = new Blob([body], { type: contentType ?? 'text/plain' });
		const url = URL.createObjectURL(blob);
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = filename;
		document.body.appendChild(anchor);
		anchor.click();
		anchor.remove();
		URL.revokeObjectURL(url);
	}
</script>

<div class="flex flex-col gap-2">
	<div class="flex flex-wrap items-center gap-2">
		{#if contentType}
			<Badge tone="neutral">{contentType}</Badge>
		{/if}
		{#if byteSize !== null && byteSize !== undefined}
			<Badge tone="neutral">{formatBytes(byteSize)}</Badge>
		{/if}
		{#if truncated}
			<Badge tone="warning">truncated</Badge>
		{/if}
		<div class="flex-1"></div>
		{#if parsed.ok}
			<button
				type="button"
				class="btn !px-2 !py-1 text-xs"
				onclick={() => (mode = mode === 'auto' ? 'raw' : 'auto')}
			>
				{mode === 'auto' ? 'View raw' : 'View JSON'}
			</button>
		{/if}
		{#if body.length > 0}
			<CopyButton text={body} />
			<button type="button" class="btn !px-2 !py-1 text-xs" onclick={download}>
				<Icon name="download" size={14} />
				Download
			</button>
		{/if}
	</div>

	{#if large}<p class="text-xs opacity-70">Showing the first 128 KiB of text. Download the captured body to inspect the rest.</p>{/if}
	{#if body.length === 0}
		<EmptyState icon="inbox" title="No body captured" message="This payload was empty or not stored." />
	{:else if showJson}
		<JsonViewer value={parsed.value} />
	{:else}
		<pre
			class="overflow-auto rounded-lg p-3 text-xs leading-relaxed"
			style="background-color: var(--color-surface-muted); max-height: 28rem; white-space: pre-wrap; word-break: break-word;">{preview}</pre>
	{/if}
</div>
