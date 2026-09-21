<script lang="ts">
	import { requestKindLabel } from '$lib/utils/format';
	import { page } from '$app/state';
	import { listReturnHref } from '$lib/utils/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import Tabs, { type Tab } from '$lib/components/Tabs.svelte';
	import KeyValue from '$lib/components/KeyValue.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import HeadersTable from '$lib/components/HeadersTable.svelte';
	import BodyViewer from '$lib/components/BodyViewer.svelte';
	import JsonViewer from '$lib/components/JsonViewer.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import CopyButton from '$lib/components/CopyButton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Skeleton from '$lib/components/Skeleton.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import * as requestsApi from '$lib/api/endpoints/requests';
	import type { RequestDetail } from '$lib/api/types';
	import { formatDateTime, formatDuration, formatMs, formatBytes, formatCost, formatTokens } from '$lib/utils/format';

	const id = $derived(page.params.id ?? '');

	const detail = createResource<RequestDetail>((signal) =>
		requestsApi.getRequest(id, signal), () => id
	);


	let activeTab = $state('overview');
    const payload = createResource<RequestDetail>((signal) => requestsApi.getRequest(page.params.id ?? '', signal, true));
    const needsPayload = $derived(['request', 'response', 'raw'].includes(activeTab));
    $effect(() => {
        if (needsPayload && payload.data?.id !== id) payload.load();
    });


	const pluginNames = $derived(Object.keys(detail.data?.plugin_metadata ?? {}));

	const tabs = $derived<Tab[]>([
		{ id: 'overview', label: 'Overview' },
		{ id: 'request', label: 'Request' },
		{ id: 'response', label: 'Response' },
		{ id: 'tools', label: `Tool calls (${detail.data?.tool_call_count ?? 0})` },
		{ id: 'plugins', label: `Plugins${pluginNames.length ? ` (${pluginNames.length})` : ''}` },
		{ id: 'raw', label: 'Raw' }
	]);

	const requestContentType = $derived.by(() => {
		const headers = detail.data?.request_headers ?? {};
		const entry = Object.entries(headers).find(([k]) => k.toLowerCase() === 'content-type');
		return typeof entry?.[1] === 'string' ? entry[1] : null;
	});

</script>

<svelte:head><title>Request · llmtrace</title></svelte:head>

<PageHeader title="Request detail" description={id}>
	{#snippet actions()}
		<a class="btn" href={listReturnHref(page.url.searchParams.get('from'), `${base}/requests`)}>
			<Icon name="chevron-left" size={16} /> Back
		</a>
		<CopyButton text={id} label="Copy ID" />
	{/snippet}
</PageHeader>

{#if detail.loading && !detail.data}
	<div class="flex flex-col gap-3">
		<Skeleton height="2.5rem" />
		<Skeleton height="16rem" />
	</div>
{:else if detail.error}
	<Card><ErrorState message={detail.error} onRetry={() => detail.load()} /></Card>
{:else if detail.data}
	{@const d = detail.data}
	<div class="mb-4 flex flex-wrap items-center gap-2">
		<StatusPill status={d.status} error={d.error} />
		<Badge tone="neutral">{d.method}</Badge>
		<Badge tone="brand">{requestKindLabel(d.request_kind)}</Badge>
		{#if d.model}<Badge tone="info">{d.model}</Badge>{/if}
		{#each d.tags as tag (tag)}<Badge tone="neutral">{tag}</Badge>{/each}
	</div>

	<Tabs {tabs} active={activeTab} onChange={(t) => (activeTab = t)} />

	<div class="mt-4">
		{#if needsPayload && payload.error}
            <Card><ErrorState message={payload.error} onRetry={() => payload.load()} /></Card>
        {:else if needsPayload && (!payload.data || payload.data.id !== id)}
            <Card><Skeleton height="12rem" /></Card>
        {:else if activeTab === 'overview'}
			<div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
				<Card title="Summary">
					<div class="grid grid-cols-1 gap-x-6 sm:grid-cols-2">
						<KeyValue label="Started" value={formatDateTime(d.started_at)} />
						<KeyValue label="Completed" value={formatDateTime(d.completed_at)} />
						<KeyValue label="Duration" value={formatDuration(d.duration_ms)} />
						<KeyValue label="First output (TTFT)" value={formatMs(d.ttft_ms)} />
						<KeyValue label="First byte (TTFB)" value={formatMs(d.ttfb_ms)} />
						<KeyValue label="Bytes in" value={formatBytes(d.bytes_in)} />
						<KeyValue label="Bytes out" value={formatBytes(d.bytes_out)} />
						<KeyValue label="Request kind" value={requestKindLabel(d.request_kind)} />
						<KeyValue label="Model" value={d.model} />
                        <KeyValue label="Input tokens" value={formatTokens(d.input_tokens)} />
                        <KeyValue label="Output tokens" value={formatTokens(d.output_tokens)} />
                        <KeyValue label="Cached input" value={formatTokens(d.cached_input_tokens)} />
                        <KeyValue label="Estimated token cost" value={formatCost(d.estimated_cost_microusd)} />
                        <KeyValue label="Payload capture" value={d.tags.includes('capture_interrupted') ? 'Interrupted — partial response' : d.request_body_truncated || d.response_body_truncated ? 'Partial — capture limit or interrupted upload' : 'No truncation recorded'} />
                        <KeyValue label="Usage coverage" value={d.usage_complete ? 'Reported by upstream' : 'Missing or partial'} />
					</div>
				</Card>
				<Card title="Routing & identity">
					<div class="grid grid-cols-1 gap-x-6">
						<KeyValue label="Method" value={d.method} mono />
						<KeyValue label="Original URI" value={d.original_uri} mono />
						<KeyValue label="Upstream URL" value={d.upstream_url} mono />
						<KeyValue label="Upstream host" value={d.upstream_host} mono />
						<KeyValue label="API key hash" value={d.api_key_hash} mono />
						<KeyValue label="Session">
							{#if d.session_id}
								<a class="hover:underline" style="color: var(--color-brand);" href={`${base}/sessions/${d.session_id}`}>
									{d.session_key ?? d.session_id}
								</a>
							{:else}
								—
							{/if}
						</KeyValue>
					</div>
				</Card>
				{#if d.error}
					<Card title="Error" class="lg:col-span-2">
						<pre class="overflow-auto rounded-lg p-3 text-xs" style="background-color: color-mix(in srgb, var(--color-danger) 10%, transparent); color: var(--color-danger); white-space: pre-wrap;">{d.error}</pre>
					</Card>
				{/if}
			</div>
		{:else if activeTab === 'request'}
			<div class="flex flex-col gap-4">
				<Card title="Request headers"><HeadersTable headers={d.request_headers} /></Card>
				<Card title="Request body" bodyClass="p-4">
					<BodyViewer
						body={payload.data?.request_body ?? ''}
						status={payload.data?.request_body_status}
						contentType={requestContentType}
						byteSize={d.request_body_bytes}
						truncated={d.request_body_truncated}
						filename={`request-${d.id}.txt`}
					/>
				</Card>
			</div>
		{:else if activeTab === 'response'}
			<div class="flex flex-col gap-4">
				<Card title="Response headers"><HeadersTable headers={d.response_headers} /></Card>
				<Card title="Response body" bodyClass="p-4">
					<BodyViewer
						body={payload.data?.response_body ?? ''}
						status={payload.data?.response_body_status}
						contentType={d.content_type}
						byteSize={d.response_body_bytes}
						truncated={d.response_body_truncated}
						filename={`response-${d.id}.txt`}
					/>
				</Card>
			</div>
		{:else if activeTab === 'tools'}
            <Card title="Generated tool calls" subtitle="Arguments are captured previews. Tool execution happens in the client or upstream.">
                {#if d.tool_calls.length === 0}
                    <EmptyState icon="box" title="No tool calls captured" message="This response has no parsed function or tool-use calls." />
                {:else}
                    {#each d.tool_calls as tool, index (index)}
                        <div class="mb-4">
                            <p class="mb-2 font-medium">{tool.name || 'Unnamed tool'} <span class="text-xs opacity-60">{tool.id}</span></p>
                            <BodyViewer body={tool.arguments} filename={`tool-${index}.json`} />
                        </div>
                    {/each}
                {/if}
            </Card>
        {:else if activeTab === 'plugins'}
			{#if pluginNames.length === 0}
				<Card><EmptyState icon="box" title="No plugin metadata" message="No plugins enriched this request." /></Card>
			{:else}
				<div class="flex flex-col gap-4">
					{#each pluginNames as name (name)}
						<Card title={name}>
							<JsonViewer value={d.plugin_metadata[name]} />
						</Card>
					{/each}
				</div>
			{/if}
		{:else if activeTab === 'raw'}
			<Card title="Raw trace JSON" bodyClass="p-4">
				<JsonViewer value={payload.data} maxHeight="40rem" />
			</Card>
		{/if}
	</div>
{/if}
