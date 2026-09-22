<script lang="ts">
	import { requestKindLabel } from '$lib/utils/format';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
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
	import ProvidedTools from '$lib/components/ProvidedTools.svelte';
	import RequestToolCalls from '$lib/components/RequestToolCalls.svelte';
	import { requestToolHistory } from '$lib/transcript/request-tools';
	import { describeTools, toolDeclarations } from '$lib/transcript/tools';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import { copyText } from '$lib/utils/clipboard';
	import { toasts } from '$lib/state/toast.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Skeleton from '$lib/components/Skeleton.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import * as requestsApi from '$lib/api/endpoints/requests';
	import type { RequestDetail } from '$lib/api/types';
	import { formatDateTime, formatDuration, formatMs, formatBytes, formatCost, formatTokens } from '$lib/utils/format';

	const id = $derived(page.params.id ?? '');

	async function copyRequestId() {
		try {
			await copyText(id);
			toasts.success('Request ID copied');
		} catch {
			toasts.error('Could not copy request ID');
		}
	}

	const detail = createResource<RequestDetail>((signal) =>
		requestsApi.getRequest(id, signal), () => id
	);


	const tabIds = ['overview', 'request', 'declared-tools', 'response', 'tools', 'plugins', 'raw'];
	const activeTab = $derived(tabIds.includes(page.url.searchParams.get('tab') ?? '') ? page.url.searchParams.get('tab')! : 'overview');
	function tabHref(tab: string) {
		const url = new URL(page.url);
		url.searchParams.set('tab', tab);
		return url.pathname + url.search;
	}
	function selectTab(tab: string) {
		void goto(tabHref(tab), { replaceState: true, noScroll: true, keepFocus: true });
	}
	const payload = createResource<RequestDetail>((signal) => requestsApi.getRequest(page.params.id ?? '', signal, true));
	const needsPayload = $derived(['request', 'declared-tools', 'response', 'raw', 'tools'].includes(activeTab));
	$effect(() => {
		if (needsPayload && payload.data?.id !== id) payload.load();
	});
	const requestData = $derived.by(() => {
		const captured = payload.data;
		if (!captured || captured.id !== id) return { value: undefined, notice: null };
		if (captured.request_body_status !== 'available') return { value: undefined, notice: 'The captured request body is unavailable; request tool information cannot be read.' };
		try {
			return {
				value: JSON.parse(captured.request_body) as unknown,
				notice: captured.request_body_truncated ? 'The request body was captured incompletely; some tool information may be unavailable.' : null
			};
		} catch {
			return { value: undefined, notice: 'The captured request body is not valid JSON; request tool information cannot be read.' };
		}
	});
	const providedTools = $derived(describeTools(toolDeclarations(requestData.value)));
	const inputToolHistory = $derived(requestToolHistory(requestData.value));

	const pluginNames = $derived(Object.keys(detail.data?.plugin_metadata ?? {}));

	const tabs = $derived<Tab[]>([
		{ id: 'overview', label: 'Overview' },
		{ id: 'request', label: 'Request' },
		{ id: 'declared-tools', label: 'Declared tools' },
		{ id: 'response', label: 'Response' },
		{ id: 'tools', label: 'Tool calls' },
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

<PageHeader title="Request detail">
	{#snippet description()}
		<button
			type="button"
			class="hover:text-fg cursor-pointer font-mono underline underline-offset-2"
			onclick={copyRequestId}
			aria-label="Copy full request ID"
			title={`Copy full request ID: ${id}`}
		>{id}</button>
	{/snippet}
	{#snippet actions()}
		<a class="btn" href={listReturnHref(page.url.searchParams.get('from'), `${base}/requests`)}>
			<Icon name="chevron-left" size={16} /> Back
		</a>
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

	<Tabs {tabs} active={activeTab} onChange={selectTab} />

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
				{#if inputToolHistory.calls.length || inputToolHistory.unmatchedResults.length}
					<RequestToolCalls history={inputToolHistory} notice={requestData.notice} />
				{/if}
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
		{:else if activeTab === 'declared-tools'}
			<div class="flex flex-col gap-4">
				{#if requestData.notice}<p class="text-fg-muted text-sm" role="status">{requestData.notice}</p>{/if}
				{#if providedTools.length || !requestData.notice}<ProvidedTools tools={providedTools} />{/if}
				{#if d.session_id}
					<a class="text-fg-muted w-fit text-sm underline underline-offset-2" href={`${base}/sessions/${d.session_id}/transcript`}>View tool declarations across this session</a>
				{/if}
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
			<div class="flex flex-col gap-4">
				<a class="text-fg-muted w-fit text-sm underline underline-offset-2" href={tabHref('declared-tools')}>View declared tools</a>
				<RequestToolCalls history={inputToolHistory} notice={requestData.notice} />
				<Card title="New tool calls in response" subtitle={`${d.tool_calls.length} new calls · arguments are captured previews`}>
					{#if d.tool_calls.length === 0}
						<EmptyState icon="box" title="No new tool calls in this response" message="Calls carried in the request history are shown above." />
					{:else}
						{#each d.tool_calls as tool, index (index)}
							<div class="mb-4">
								<p class="mb-2 font-medium">{tool.name || 'Unnamed tool'} <span class="text-xs opacity-60">{tool.id}</span></p>
								<BodyViewer body={tool.arguments} filename={`tool-${index}.json`} />
							</div>
						{/each}
					{/if}
				</Card>
			</div>
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
