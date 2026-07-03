<script lang="ts">
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import Field from '$lib/components/Field.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import HeadersTable from '$lib/components/HeadersTable.svelte';
	import KeyValue from '$lib/components/KeyValue.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { ApiError } from '$lib/api/client';
	import * as admin from '$lib/api/endpoints/admin';
	import type { RedactionPreview, RedactionPreviewRequest } from '$lib/api/types';
	import { formatBytes } from '$lib/utils/format';

	const SAMPLE_HEADERS = `authorization: Bearer sk-secret-value-1234567890\ncontent-type: application/json\nx-api-key: my-api-key-abcdef`;

	let headersText = $state(SAMPLE_HEADERS);
	let uri = $state('/v1/chat/completions?api_key=secret123');
	let body = $state('{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}');

	let running = $state(false);
	let error = $state<string | null>(null);
	let preview = $state<RedactionPreview | undefined>();

	function parseHeaders(text: string): Record<string, string> {
		const out: Record<string, string> = {};
		for (const line of text.split('\n')) {
			const trimmed = line.trim();
			if (!trimmed) continue;
			const idx = trimmed.indexOf(':');
			if (idx <= 0) continue;
			const name = trimmed.slice(0, idx).trim();
			const value = trimmed.slice(idx + 1).trim();
			if (name) out[name] = value;
		}
		return out;
	}

	async function run() {
		running = true;
		error = null;
		const request: RedactionPreviewRequest = {};
		const headers = parseHeaders(headersText);
		if (Object.keys(headers).length) request.headers = headers;
		if (uri.trim()) request.uri = uri;
		if (body.trim()) request.body = body;
		try {
			preview = await admin.redactionPreview(request);
		} catch (err) {
			error = err instanceof ApiError || err instanceof Error ? err.message : 'Preview failed.';
		} finally {
			running = false;
		}
	}
</script>

<svelte:head><title>Redaction · llmtrace</title></svelte:head>

<PageHeader title="Redaction preview" description="Test how headers, URIs, and bodies are redacted before persistence." />

<div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
	<Card title="Input">
		<div class="flex flex-col gap-4">
			<Field label="Headers" hint="One per line, as Name: value">
				<textarea class="input font-mono text-xs" rows="5" bind:value={headersText}></textarea>
			</Field>
			<Field label="URI">
				<input class="input font-mono text-xs" type="text" bind:value={uri} />
			</Field>
			<Field label="Body">
				<textarea class="input font-mono text-xs" rows="5" bind:value={body}></textarea>
			</Field>
			<button type="button" class="btn btn-brand self-start" onclick={run} disabled={running}>
				{#if running}<Icon name="refresh" size={16} class="animate-spin" /> Previewing…{:else}<Icon name="eye-off" size={16} /> Preview redaction{/if}
			</button>
			{#if error}
				<p class="text-sm" style="color: var(--color-danger);">{error}</p>
			{/if}
		</div>
	</Card>

	<div class="flex flex-col gap-4">
		{#if !preview}
			<Card><EmptyState icon="eye-off" title="No preview yet" message="Enter sample data and run the preview." /></Card>
		{:else}
			<Card title="Redaction settings">
				<div class="grid grid-cols-2 gap-x-4">
					<KeyValue label="Store header hash" value={preview.redaction.store_header_hash ? 'yes' : 'no'} />
					<KeyValue label="Sensitive headers" value={preview.redaction.sensitive_header_count} />
					<KeyValue label="Upstream header" value={preview.redaction.upstream_header} mono />
				</div>
				{#if Object.keys(preview.redaction.limits).length > 0}
					<div class="mt-2 flex flex-wrap gap-2">
						{#each Object.entries(preview.redaction.limits) as [name, value] (name)}
							<Badge tone="neutral">{name}: {formatBytes(value)}</Badge>
						{/each}
					</div>
				{/if}
			</Card>

			{#if preview.headers.provided}
				<Card title="Redacted headers">
					<HeadersTable headers={preview.headers.redacted} />
					{#if preview.headers.first_secret_header_hash}
						<p class="text-fg-muted mt-2 font-mono text-xs">first secret hash: {preview.headers.first_secret_header_hash}</p>
					{/if}
				</Card>
			{/if}

			{#if preview.uri.provided}
				<Card title="Redacted URI">
					{#snippet actions()}
						{#if preview?.uri.changed}<Badge tone="warning">changed</Badge>{:else}<Badge tone="neutral">unchanged</Badge>{/if}
					{/snippet}
					<pre class="overflow-x-auto rounded-lg p-2 text-xs" style="background-color: var(--color-surface-muted);">{preview.uri.redacted}</pre>
					<p class="text-fg-muted mt-1 text-xs">{preview.uri.input_bytes ?? 0} → {preview.uri.output_bytes ?? 0} bytes</p>
				</Card>
			{/if}

			{#if preview.body.provided}
				<Card title="Redacted body">
					{#snippet actions()}
						{#if preview?.body.dropped}<Badge tone="danger">dropped</Badge>{:else if preview?.body.changed}<Badge tone="warning">changed</Badge>{:else}<Badge tone="neutral">unchanged</Badge>{/if}
					{/snippet}
					{#if preview.body.dropped}
						<p class="text-fg-muted text-sm">Body was dropped from storage.</p>
					{:else}
						<pre class="max-h-80 overflow-auto rounded-lg p-2 text-xs" style="background-color: var(--color-surface-muted);">{preview.body.redacted}</pre>
					{/if}
					<p class="text-fg-muted mt-1 text-xs">{preview.body.input_bytes ?? 0} → {preview.body.output_bytes ?? 0} bytes</p>
				</Card>
			{/if}
		{/if}
	</div>
</div>
