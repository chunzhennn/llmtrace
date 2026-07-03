<script lang="ts">
	import { onMount } from 'svelte';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import Card from '$lib/components/Card.svelte';
	import Chart from '$lib/components/Chart.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import StatusPill from '$lib/components/StatusPill.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import MiniBar from '$lib/components/MiniBar.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as analytics from '$lib/api/endpoints/analytics';
	import * as requestsApi from '$lib/api/endpoints/requests';
	import * as admin from '$lib/api/endpoints/admin';
	import type {
		Stats,
		UsageSummary,
		UsageTimeseries,
		RecentErrorsResponse,
		SlowRequestsResponse,
		SecurityPosture,
		RetentionStatus,
		RequestSummary
	} from '$lib/api/types';
	import {
		formatNumber,
		formatBytes,
		formatMs,
		formatPercent,
		formatDateTime,
		formatDuration
	} from '$lib/utils/format';
	import { baseTimeChartOptions, chartColors, withAlpha } from '$lib/utils/chart';
	import type { ChartData } from 'chart.js';

	interface Core {
		stats: Stats;
		summary: UsageSummary;
		timeseries: UsageTimeseries;
	}

	const core = new Resource<Core>(async (signal) => {
		const [stats, summary, timeseries] = await Promise.all([
			analytics.stats(signal),
			analytics.usageSummary({ since_hours: 24 }, signal),
			analytics.usageTimeseries({ since_hours: 24, bucket: 'hour' }, signal)
		]);
		return { stats, summary, timeseries };
	});

	const recentErrors = new Resource<RecentErrorsResponse>((signal) =>
		requestsApi.recentErrors({ since_hours: 24, limit: 8 }, signal)
	);
	const slow = new Resource<SlowRequestsResponse>((signal) =>
		requestsApi.slowRequests({ since_hours: 24, limit: 8 }, signal)
	);

	interface Ops {
		posture: SecurityPosture;
		retention: RetentionStatus;
	}
	const ops = new Resource<Ops>(async (signal) => {
		const [posture, retention] = await Promise.all([
			admin.securityPosture(signal),
			admin.retentionStatus(signal)
		]);
		return { posture, retention };
	});

	onMount(() => {
		core.load();
		recentErrors.load();
		slow.load();
		ops.load();
	});

	const stats = $derived(core.data?.stats);
	const errorRate = $derived(
		stats && stats.total > 0 ? stats.errors / stats.total : 0
	);
	const pipeline = $derived(stats?.runtime.trace_pipeline);
	const dropped = $derived(
		pipeline ? pipeline.dropped_full + pipeline.dropped_closed : 0
	);

	const chartData = $derived.by<ChartData>(() => {
		const points = core.data?.timeseries.points ?? [];
		const colors = chartColors();
		return {
			labels: points.map((p) =>
				new Date(p.bucket).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
			),
			datasets: [
				{
					label: 'Requests',
					data: points.map((p) => p.request_count),
					borderColor: colors.brand,
					backgroundColor: withAlpha(colors.brand, 0.15),
					fill: true,
					tension: 0.3,
					pointRadius: 0,
					borderWidth: 2
				},
				{
					label: 'Errors',
					data: points.map((p) => p.error_count),
					borderColor: colors.danger,
					backgroundColor: withAlpha(colors.danger, 0.12),
					fill: true,
					tension: 0.3,
					pointRadius: 0,
					borderWidth: 2
				}
			]
		};
	});

	const chartOptions = $derived(baseTimeChartOptions());

	const modelMax = $derived(
		Math.max(1, ...(core.data?.summary.top_models ?? []).map((m) => m.request_count))
	);
	const upstreamMax = $derived(
		Math.max(1, ...(core.data?.summary.top_upstreams ?? []).map((m) => m.request_count))
	);

	const errorColumns: Column[] = [
		{ label: 'Time' },
		{ label: 'Status' },
		{ label: 'Model' },
		{ label: 'Upstream' },
		{ label: 'Duration', align: 'right' }
	];
	const slowColumns: Column[] = [
		{ label: 'Time' },
		{ label: 'Duration', align: 'right' },
		{ label: 'Status' },
		{ label: 'Model' },
		{ label: 'Upstream' }
	];

	function requestHref(row: RequestSummary): string {
		return `${base}/requests/${row.id}`;
	}

	const postureTone = { ready: 'success', attention: 'warning', fail: 'danger' } as const;
</script>

<svelte:head><title>Overview · llmtrace</title></svelte:head>

<PageHeader title="Overview" description="Live snapshot of proxied LLM traffic over the last 24 hours." />

{#if core.error}
	<Card>
		<div class="flex items-center justify-between gap-3">
			<span class="text-sm" style="color: var(--color-danger);">{core.error}</span>
			<button type="button" class="btn" onclick={() => core.load()}>
				<Icon name="refresh" size={16} /> Retry
			</button>
		</div>
	</Card>
{:else}
	<div class="grid grid-cols-2 gap-3 lg:grid-cols-3 xl:grid-cols-6">
		<StatCard label="Total requests" value={formatNumber(stats?.total ?? 0)} icon="list" loading={core.loading} />
		<StatCard label="Last hour" value={formatNumber(stats?.last_hour ?? 0)} icon="clock" loading={core.loading} />
		<StatCard
			label="Errors (all)"
			value={formatNumber(stats?.errors ?? 0)}
			icon="alert"
			tone={stats && stats.errors > 0 ? 'danger' : 'default'}
			hint={`${formatPercent(errorRate)} error rate`}
			loading={core.loading}
		/>
		<StatCard label="Captured" value={formatBytes(stats?.captured_bytes ?? 0)} icon="database" loading={core.loading} />
		<StatCard label="Avg duration" value={formatMs(stats?.avg_duration_ms)} icon="activity" loading={core.loading} />
		<StatCard label="Avg TTFT" value={formatMs(stats?.avg_ttft_ms)} icon="activity" loading={core.loading} />
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 xl:grid-cols-3">
		<div class="xl:col-span-2">
			<Card title="Requests & errors (24h, hourly)">
				{#if core.loading}
					<div class="h-64 animate-pulse rounded" style="background-color: var(--color-surface-muted);"></div>
				{:else}
					<Chart type="line" data={chartData} options={chartOptions} height="16rem" />
				{/if}
			</Card>
		</div>

		<Card title="Operational health">
			<div class="flex flex-col gap-3 text-sm">
				<div class="flex items-center justify-between">
					<span class="text-fg-muted">Security posture</span>
					{#if ops.data}
						<Badge tone={postureTone[ops.data.posture.overall]}>
							{ops.data.posture.overall}
							· {ops.data.posture.counts.warn}w / {ops.data.posture.counts.fail}f
						</Badge>
					{:else}
						<span class="text-fg-muted">…</span>
					{/if}
				</div>
				<div class="flex items-center justify-between">
					<span class="text-fg-muted">Trace queue depth</span>
					<span class="tabular-nums">
						{pipeline ? `${formatNumber(pipeline.queue_depth)} / ${formatNumber(pipeline.queue_capacity)}` : '—'}
					</span>
				</div>
				<div class="flex items-center justify-between">
					<span class="text-fg-muted">Dropped traces</span>
					<span class="tabular-nums" style={dropped > 0 ? 'color: var(--color-warning);' : ''}>
						{formatNumber(dropped)}
					</span>
				</div>
				<div class="flex items-center justify-between">
					<span class="text-fg-muted">Persisted traces</span>
					<span class="tabular-nums">{formatNumber(pipeline?.persisted ?? 0)}</span>
				</div>
				<div class="flex items-center justify-between">
					<span class="text-fg-muted">Retention</span>
					{#if ops.data}
						<span>
							{#if ops.data.retention.enabled}
								<Badge tone="info">{ops.data.retention.retention_days}d</Badge>
							{:else}
								<Badge tone="warning">disabled</Badge>
							{/if}
						</span>
					{:else}
						<span class="text-fg-muted">…</span>
					{/if}
				</div>
			</div>
		</Card>
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
		<Card title="Top models (24h)">
			{#if (core.data?.summary.top_models.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No traffic in the last 24 hours.</p>
			{:else}
				<div class="flex flex-col gap-3">
					{#each core.data?.summary.top_models ?? [] as model (model.name)}
						<MiniBar
							label={model.name}
							value={model.request_count}
							max={modelMax}
							display={`${formatNumber(model.request_count)}${model.error_count > 0 ? ` · ${model.error_count} err` : ''}`}
							href={`${base}/requests?model=${encodeURIComponent(model.name)}`}
						/>
					{/each}
				</div>
			{/if}
		</Card>

		<Card title="Top upstreams (24h)">
			{#if (core.data?.summary.top_upstreams.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No traffic in the last 24 hours.</p>
			{:else}
				<div class="flex flex-col gap-3">
					{#each core.data?.summary.top_upstreams ?? [] as up (up.name)}
						<MiniBar
							label={up.name}
							value={up.request_count}
							max={upstreamMax}
							tone="var(--color-info)"
							display={`${formatNumber(up.request_count)}${up.error_count > 0 ? ` · ${up.error_count} err` : ''}`}
							href={`${base}/requests?upstream_host=${encodeURIComponent(up.name)}`}
						/>
					{/each}
				</div>
			{/if}
		</Card>
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 xl:grid-cols-2">
		<div>
			<div class="mb-2 flex items-center justify-between">
				<h2 class="text-sm font-semibold">Recent errors (24h)</h2>
				<a class="text-xs hover:underline" style="color: var(--color-brand);" href={`${base}/requests?has_error=true`}>View all</a>
			</div>
			<DataTable
				columns={errorColumns}
				rows={recentErrors.data?.items ?? []}
				loading={recentErrors.loading}
				error={recentErrors.error}
				emptyTitle="No errors"
				emptyMessage="No failed requests in the last 24 hours."
				onRetry={() => recentErrors.load()}
			>
				{#snippet row(item: RequestSummary)}
					<td class="px-3 py-2 whitespace-nowrap">
						<a class="hover:underline" style="color: var(--color-brand);" href={requestHref(item)}>
							{formatDateTime(item.started_at)}
						</a>
					</td>
					<td class="px-3 py-2"><StatusPill status={item.status} error={item.error} /></td>
					<td class="px-3 py-2">{item.model ?? '—'}</td>
					<td class="px-3 py-2 max-w-[12rem] truncate" title={item.upstream_host ?? ''}>{item.upstream_host ?? '—'}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatDuration(item.duration_ms)}</td>
				{/snippet}
			</DataTable>
		</div>

		<div>
			<div class="mb-2 flex items-center justify-between">
				<h2 class="text-sm font-semibold">Slowest requests (24h)</h2>
				<a class="text-xs hover:underline" style="color: var(--color-brand);" href={`${base}/requests`}>View all</a>
			</div>
			<DataTable
				columns={slowColumns}
				rows={slow.data?.items ?? []}
				loading={slow.loading}
				error={slow.error}
				emptyTitle="No requests"
				onRetry={() => slow.load()}
			>
				{#snippet row(item: RequestSummary)}
					<td class="px-3 py-2 whitespace-nowrap">
						<a class="hover:underline" style="color: var(--color-brand);" href={requestHref(item)}>
							{formatDateTime(item.started_at)}
						</a>
					</td>
					<td class="px-3 py-2 text-right font-medium tabular-nums">{formatDuration(item.duration_ms)}</td>
					<td class="px-3 py-2"><StatusPill status={item.status} error={item.error} /></td>
					<td class="px-3 py-2">{item.model ?? '—'}</td>
					<td class="px-3 py-2 max-w-[12rem] truncate" title={item.upstream_host ?? ''}>{item.upstream_host ?? '—'}</td>
				{/snippet}
			</DataTable>
		</div>
	</div>
{/if}
