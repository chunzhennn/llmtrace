<script lang="ts">
	import { onMount } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import KeyValue from '$lib/components/KeyValue.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as admin from '$lib/api/endpoints/admin';
	import * as analytics from '$lib/api/endpoints/analytics';
	import type {
		RuntimeConfig,
		SecurityPosture,
		RetentionStatus,
		StorageSummary,
		StorageRelation,
		Stats
	} from '$lib/api/types';
	import { formatNumber, formatBytes, formatDateTime, formatSecondsDuration } from '$lib/utils/format';

	const config = new Resource<RuntimeConfig>((s) => admin.runtimeConfig(s));
	const posture = new Resource<SecurityPosture>((s) => admin.securityPosture(s));
	const retention = new Resource<RetentionStatus>((s) => admin.retentionStatus(s));
	const storage = new Resource<StorageSummary>((s) => admin.storageSummary(s));
	const stats = new Resource<Stats>((s) => analytics.stats(s));

	onMount(() => {
		config.load();
		posture.load();
		retention.load();
		storage.load();
		stats.load();
	});

	const postureTone = { pass: 'success', warn: 'warning', fail: 'danger' } as const;
	const overallTone = { ready: 'success', attention: 'warning', fail: 'danger' } as const;

	const pipeline = $derived(stats.data?.runtime.trace_pipeline);
    const journal = $derived(stats.data?.runtime.trace_journal);
	const retentionRuntime = $derived(stats.data?.runtime.retention);

	const storageColumns: Column[] = [
		{ label: 'Relation' },
		{ label: 'Rows', align: 'right' },
		{ label: 'Total', align: 'right' },
		{ label: 'Table', align: 'right' },
		{ label: 'Index', align: 'right' }
	];
</script>

<svelte:head><title>System · llmtrace</title></svelte:head>

<PageHeader title="System" description="Read-only runtime configuration, security posture, retention, and storage." />

<!-- Security posture -->
<Card title="Security posture">
	{#snippet actions()}
		{#if posture.data}
			<Badge tone={overallTone[posture.data.overall]}>
				{posture.data.overall} · {posture.data.counts.pass}✓ {posture.data.counts.warn}! {posture.data.counts.fail}✗
			</Badge>
		{/if}
	{/snippet}
	{#if posture.loading}
		<p class="text-fg-muted text-sm">Loading…</p>
	{:else if posture.error}
		<p class="text-sm" style="color: var(--color-danger);">{posture.error}</p>
	{:else if posture.data}
		<div class="flex flex-col gap-2">
			{#each posture.data.checks as check (check.id)}
				<div class="flex items-start gap-3">
					<Badge tone={postureTone[check.status]}>{check.status}</Badge>
					<div class="min-w-0">
						<div class="font-mono text-xs">{check.id}</div>
						<div class="text-sm">{check.message}</div>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</Card>

<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
	<!-- Runtime config -->
	<Card title="Configuration">
		{#if config.data}
			{@const c = config.data}
			<div class="grid grid-cols-2 gap-x-4">
				<KeyValue label="Deployment" value={c.server.deployment} />
				<KeyValue label="Admin URL" value={c.server.public_url} mono />
				<KeyValue label="Proxy URL" value={c.server.proxy_public_url ?? c.server.public_url} mono />
				<KeyValue label="Proxy listener" value={c.server.listen} mono />
				<KeyValue label="Admin listener" value={c.server.admin_listen ?? 'shared with proxy'} mono />
				<KeyValue label="UI enabled" value={c.server.ui_enabled ? 'yes' : 'no'} />
				<KeyValue label="Default upstream" value={c.proxy.default_upstream} mono />
				<KeyValue label="Routing preset" value={c.proxy.preset} />
				<details class="col-span-2 mb-3 rounded-lg border border-border p-3">
					<summary class="cursor-pointer text-sm text-muted focus-visible:outline-2 focus-visible:outline-accent">
						Routing prefixes: {c.proxy.path_prefixes.length} forwarded, {c.proxy.capture_path_prefixes.length} captured
					</summary>
					<div class="mt-3">
						<KeyValue label="Forwarded prefixes" value={c.proxy.path_prefixes.join(', ') || 'none'} mono />
						<KeyValue label="Captured prefixes" value={c.proxy.capture_path_prefixes.join(', ') || 'capture disabled'} mono />
					</div>
				</details>
				<KeyValue label="Upstream header" value={c.proxy.upstream_header} mono />
				<KeyValue label="Allowed upstreams" value={c.proxy.allow_upstreams_count} />
				<KeyValue label="Proxy timeout" value={`${c.proxy.timeout_secs}s`} />
				<KeyValue label="Max req body" value={formatBytes(c.proxy.max_request_body_bytes)} />
				<KeyValue label="Max resp body" value={formatBytes(c.proxy.max_response_body_bytes)} />
				<KeyValue label="Archive backend" value={c.archive.storage_backend} />
				<KeyValue label="Segment size" value={formatBytes(c.archive.segment_uncompressed_bytes)} />
				<KeyValue label="DB connections" value={c.storage.max_connections} />
				<KeyValue label="Trace workers" value={c.storage.trace_worker_count} />
				<KeyValue label="Queue capacity" value={formatNumber(c.storage.trace_queue_capacity)} />
				<KeyValue label="Trace memory budget" value={formatBytes(c.storage.trace_queue_max_bytes)} />
				<KeyValue label="Retention days" value={c.storage.retention_days ?? 'disabled'} />
				<KeyValue label="Archive size limit" value={c.storage.rotate_size_bytes > 0 ? formatBytes(c.storage.rotate_size_bytes) : 'disabled'} />
				<KeyValue label="Sensitive headers" value={c.redaction.sensitive_header_count} />
				<KeyValue label="Body storage" value={c.redaction.body_storage} />
				<KeyValue label="Store header hash" value={c.redaction.store_header_hash ? 'yes' : 'no'} />
				<KeyValue label="Metrics token" value={c.observability.metrics_bearer_token_configured ? 'configured' : 'none'} />
			</div>
		{:else if config.error}
			<p class="text-sm" style="color: var(--color-danger);">{config.error}</p>
		{:else}
			<p class="text-fg-muted text-sm">Loading…</p>
		{/if}
	</Card>

	<!-- Runtime metrics -->
	<Card title="Runtime">
		{#if stats.data}
			<div class="grid grid-cols-2 gap-x-4">
				<KeyValue label="Queue depth" value={pipeline ? `${formatNumber(pipeline.queue_depth)} / ${formatNumber(pipeline.queue_capacity)}` : '—'} />
				<KeyValue label="Enqueued" value={formatNumber(pipeline?.enqueued ?? 0)} />
				<KeyValue label="Persisted" value={formatNumber(pipeline?.persisted ?? 0)} />
				<KeyValue label="Dropped (full)" value={formatNumber(pipeline?.dropped_full ?? 0)} />
				<KeyValue label="Dropped (memory)" value={formatNumber(pipeline?.dropped_memory ?? 0)} />
				<KeyValue label="Trace event memory" value={pipeline ? `${formatBytes(pipeline.memory_used_bytes)} / ${formatBytes(pipeline.memory_limit_bytes)}` : '—'} />
				<KeyValue label="Dropped (closed)" value={formatNumber(pipeline?.dropped_closed ?? 0)} />
				<KeyValue label="Build failures" value={formatNumber(pipeline?.build_failures ?? 0)} />
				<KeyValue label="Persist failures" value={formatNumber(pipeline?.persist_failures ?? 0)} />
				<KeyValue label="Retention runs" value={formatNumber(retentionRuntime?.runs ?? 0)} />
				<KeyValue label="Retention failures" value={formatNumber(retentionRuntime?.failures ?? 0)} />
				<KeyValue label="Last prune ok" value={formatDateTime(retentionRuntime?.last_success_at)} />
			</div>
			{#if retentionRuntime?.last_error}
				<p class="mt-2 text-xs" style="color: var(--color-danger);">Last error: {retentionRuntime.last_error}</p>
			{/if}
		{:else if stats.error}
			<p class="text-sm" style="color: var(--color-danger);">{stats.error}</p>
		{:else}
			<p class="text-fg-muted text-sm">Loading…</p>
		{/if}
	</Card>
</div>

<Card title="Capture journal" class="mt-4" subtitle="Queued captures become recoverable after a background disk sync. Live streams and captures awaiting that sync can be lost if the process stops.">
    {#if journal}
        <div class="grid grid-cols-2 gap-x-4">
            <KeyValue label="Durable journal" value={journal.enabled ? 'Enabled' : 'Disabled — captures are held in memory'} />
            <KeyValue label="Awaiting disk sync" value={formatNumber(journal.not_yet_durable)} />
            <KeyValue label="Pending persistence / cleanup" value={formatNumber(journal.pending_records)} />
            <KeyValue label="Journal size" value={`${formatBytes(journal.pending_bytes + journal.quarantined_bytes)} / ${formatBytes(journal.max_bytes)}`} />
            <KeyValue label="Oldest pending capture" value={`${journal.oldest_pending_age_secs}s`} />
            <KeyValue label="Recovered after restart" value={formatNumber(journal.recovered)} />
            <KeyValue label="Rejected (journal full)" value={formatNumber(journal.dropped_full)} />
            <KeyValue label="Disk write failures" value={formatNumber(journal.write_failed)} />
            <KeyValue label="Retry attempts" value={formatNumber(journal.retry)} />
            <KeyValue label="Quarantined records" value={formatNumber(journal.quarantined_records)} />
            <KeyValue label="Exceeding replay memory limit" value={formatNumber(journal.blocked_records)} />
        </div>
        {#if journal.quarantined_records || journal.blocked_records || journal.dropped_full || journal.write_failed}
            <p class="mt-3 text-sm" style="color: var(--color-danger);">Audit coverage needs attention. Check journal capacity, filesystem health, quarantined files, and the replay memory budget. Pending records are preserved.</p>
        {/if}
        <p class="mt-3 text-xs text-fg-muted">Failure, rejection, and recovery counters reset when this instance restarts. Pending and quarantined files are rediscovered from disk. A successful proxied response does not confirm that its audit capture is durable.</p>
    {/if}
</Card>

<!-- Retention -->
<Card title="Retention" class="mt-4">
	{#if retention.data}
		{@const r = retention.data}
		<div class="grid grid-cols-2 gap-x-4 md:grid-cols-4">
			<KeyValue label="Status">
				{#if r.enabled}<Badge tone="info">{r.retention_days}d</Badge>{:else}<Badge tone="warning">disabled</Badge>{/if}
			</KeyValue>
			<KeyValue label="Cutoff" value={formatDateTime(r.cutoff)} />
			<KeyValue label="Prune interval" value={formatSecondsDuration(r.prune_interval_secs)} />
			<KeyValue label="Batch size" value={formatNumber(r.prune_batch_size)} />
		</div>
		{#if Object.keys(r.expired).length > 0}
			<div class="mt-3">
				<span class="label">Expired rows pending prune</span>
				<div class="mt-1 flex flex-wrap gap-2">
					{#each Object.entries(r.expired) as [name, count] (name)}
						<Badge tone={count > 0 ? 'warning' : 'neutral'}>{name}: {formatNumber(count)}</Badge>
					{/each}
				</div>
			</div>
		{/if}
		<div class="mt-4 border-t border-border pt-4">
			<div class="mb-2 flex items-center gap-2">
				<h3 class="text-sm font-semibold">Archive size rotation</h3>
				<Badge tone={r.rotation.over_limit ? 'warning' : r.rotation.enabled ? 'success' : 'neutral'}>
					{r.rotation.over_limit ? 'cleanup pending' : r.rotation.enabled ? 'enabled' : 'disabled'}
				</Badge>
			</div>
			<div class="grid grid-cols-2 gap-x-4 md:grid-cols-4">
				<KeyValue label="Compressed payloads" value={formatBytes(r.rotation.retained_bytes)} />
				<KeyValue label="Size limit" value={r.rotation.enabled ? formatBytes(r.rotation.size_bytes) : 'disabled'} />
				<KeyValue label="Check interval" value={formatSecondsDuration(r.rotation.check_interval_secs)} />
				<KeyValue label="Files pending deletion" value={`${formatNumber(r.rotation.pending_delete_files)} · ${formatBytes(r.rotation.pending_delete_bytes)}`} />
			</div>
			<p class="text-fg-muted mt-2 text-xs">Removes the oldest requests and their payloads when compressed archives exceed the limit. Database metadata and operational logs are excluded. Set the limit in your configuration file or environment and restart to apply it.</p>
		</div>
	{:else if retention.error}
		<p class="text-sm" style="color: var(--color-danger);">{retention.error}</p>
	{:else}
		<p class="text-fg-muted text-sm">Loading…</p>
	{/if}
</Card>

<!-- Storage -->
<div class="mt-4">
	<div class="mb-2 flex items-center justify-between">
		<h2 class="text-sm font-semibold">Storage</h2>
		{#if storage.data}
			<span class="text-fg-muted text-xs">
				{formatBytes(storage.data.totals.total_bytes)} across {storage.data.totals.present_relation_count} relations
			</span>
		{/if}
	</div>
	<DataTable
		columns={storageColumns}
		rows={storage.data?.relations ?? []}
		loading={storage.loading}
		error={storage.error}
		emptyTitle="No relations"
		onRetry={() => storage.load()}
	>
		{#snippet row(item: StorageRelation)}
			<td class="px-3 py-2 font-mono text-xs">
				{item.name}
				{#if !item.present}<Badge tone="neutral">missing</Badge>{/if}
			</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.live_rows_estimate)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatBytes(item.total_bytes)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatBytes(item.table_bytes)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatBytes(item.index_bytes)}</td>
		{/snippet}
	</DataTable>
</div>
