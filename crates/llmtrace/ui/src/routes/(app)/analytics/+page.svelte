<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import Chart from '$lib/components/Chart.svelte';
	import Tabs, { type Tab } from '$lib/components/Tabs.svelte';
	import TimeWindowSelect from '$lib/components/TimeWindowSelect.svelte';
	import DataTable, { type Column } from '$lib/components/DataTable.svelte';
	import MiniBar from '$lib/components/MiniBar.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as analytics from '$lib/api/endpoints/analytics';
	import type {
		UsageSummary,
		UsageTimeseries,
		LatencySummary,
		LatencyMetric,
		ApiKeyUsage,
		ApiKeyUsageItem,
		UserUsage,
		UserUsageItem,
		ModelUsage,
		ModelUsageItem,
		UpstreamHealth,
		UpstreamHealthItem,
		ErrorSummary
	} from '$lib/api/types';
	import { formatNumber, formatBytes, formatMs, formatPercent, truncateMiddle } from '$lib/utils/format';
	import { baseTimeChartOptions, chartColors, withAlpha } from '$lib/utils/chart';
	import type { ChartData } from 'chart.js';

	const TABLE_LIMIT = 100;
	const DEFAULT_HOURS = 24;
	const DEFAULT_TAB = 'summary';
	const DEFAULT_BUCKET = 'hour';

	const hours = $derived(Number(page.url.searchParams.get('hours') ?? DEFAULT_HOURS) || DEFAULT_HOURS);
	const tab = $derived(page.url.searchParams.get('tab') ?? DEFAULT_TAB);
	const bucket = $derived(page.url.searchParams.get('bucket') ?? DEFAULT_BUCKET);

	const tabs: Tab[] = [
		{ id: 'summary', label: 'Summary' },
		{ id: 'timeseries', label: 'Timeseries' },
		{ id: 'latency', label: 'Latency' },
		{ id: 'api-keys', label: 'API keys' },
		{ id: 'users', label: 'Users' },
		{ id: 'models', label: 'Models' },
		{ id: 'upstreams', label: 'Upstreams' },
		{ id: 'errors', label: 'Errors' }
	];

	const summary = new Resource<UsageSummary>((signal) => analytics.usageSummary({ since_hours: hours }, signal));
	const timeseries = new Resource<UsageTimeseries>((signal) =>
		analytics.usageTimeseries({ since_hours: hours, bucket }, signal)
	);
	const latency = new Resource<LatencySummary>((signal) => analytics.latencySummary({ since_hours: hours }, signal));
	const apiKeys = new Resource<ApiKeyUsage>((signal) =>
		analytics.apiKeyUsage({ since_hours: hours, limit: TABLE_LIMIT }, signal)
	);
	const users = new Resource<UserUsage>((signal) =>
		analytics.userUsage({ since_hours: hours, limit: TABLE_LIMIT }, signal)
	);
	const models = new Resource<ModelUsage>((signal) =>
		analytics.modelUsage({ since_hours: hours, limit: TABLE_LIMIT }, signal)
	);
	const upstreams = new Resource<UpstreamHealth>((signal) =>
		analytics.upstreamHealth({ since_hours: hours, limit: TABLE_LIMIT }, signal)
	);
	const errors = new Resource<ErrorSummary>((signal) => analytics.errorSummary({ since_hours: hours }, signal));

	$effect(() => {
		void hours;
		void bucket;
		switch (tab) {
			case 'summary':
				summary.load();
				break;
			case 'timeseries':
				timeseries.load();
				break;
			case 'latency':
				latency.load();
				break;
			case 'api-keys':
				apiKeys.load();
				break;
			case 'users':
				users.load();
				break;
			case 'models':
				models.load();
				break;
			case 'upstreams':
				upstreams.load();
				break;
			case 'errors':
				errors.load();
				break;
		}
	});

	function setParam(key: string, value: string, fallback: string) {
		const params = new URLSearchParams(page.url.searchParams);
		if (value === fallback) params.delete(key);
		else params.set(key, value);
		const qs = params.toString();
		goto(`${base}/analytics${qs ? `?${qs}` : ''}`, { keepFocus: true, noScroll: true });
	}

	const buckets = [
		{ id: 'minute', label: 'Minute' },
		{ id: 'hour', label: 'Hour' },
		{ id: 'day', label: 'Day' }
	];

	function bucketLabel(value: string): { hour: '2-digit'; minute: '2-digit' } | { month: 'short'; day: '2-digit' } {
		return value === 'day' ? { month: 'short', day: '2-digit' } : { hour: '2-digit', minute: '2-digit' };
	}

	const chartData = $derived.by<ChartData>(() => {
		const points = timeseries.data?.points ?? [];
		const colors = chartColors();
		const fmt = bucketLabel(bucket);
		return {
			labels: points.map((p) => new Date(p.bucket).toLocaleString(undefined, fmt)),
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

	const latencyChartData = $derived.by<ChartData>(() => {
		const points = timeseries.data?.points ?? [];
		const colors = chartColors();
		const fmt = bucketLabel(bucket);
		return {
			labels: points.map((p) => new Date(p.bucket).toLocaleString(undefined, fmt)),
			datasets: [
				{
					label: 'Avg duration (ms)',
					data: points.map((p) => p.avg_duration_ms ?? 0),
					borderColor: colors.info,
					backgroundColor: withAlpha(colors.info, 0.12),
					fill: true,
					tension: 0.3,
					pointRadius: 0,
					borderWidth: 2
				},
				{
					label: 'Avg TTFT (ms)',
					data: points.map((p) => p.avg_ttft_ms ?? 0),
					borderColor: colors.warning,
					backgroundColor: withAlpha(colors.warning, 0.1),
					fill: true,
					tension: 0.3,
					pointRadius: 0,
					borderWidth: 2
				}
			]
		};
	});

	const chartOptions = $derived(baseTimeChartOptions());

	const summaryTotals = $derived(summary.data?.totals);
	const modelMax = $derived(Math.max(1, ...(summary.data?.top_models ?? []).map((m) => m.request_count)));
	const upstreamMax = $derived(Math.max(1, ...(summary.data?.top_upstreams ?? []).map((m) => m.request_count)));

	const latencyColumns: Column[] = [
		{ label: 'Name' },
		{ label: 'Requests', align: 'right' },
		{ label: 'p50', align: 'right' },
		{ label: 'p90', align: 'right' },
		{ label: 'p95', align: 'right' },
		{ label: 'p99', align: 'right' },
		{ label: 'Max', align: 'right' }
	];
	const apiKeyColumns: Column[] = [
		{ label: 'API key hash' },
		{ label: 'Requests', align: 'right' },
		{ label: 'Errors', align: 'right' },
		{ label: 'Sessions', align: 'right' },
		{ label: 'Captured', align: 'right' },
		{ label: 'Avg', align: 'right' }
	];
	const userColumns: Column[] = [
		{ label: 'User' },
		{ label: 'Requests', align: 'right' },
		{ label: 'Error rate', align: 'right' },
		{ label: 'Keys', align: 'right' },
		{ label: 'Sessions', align: 'right' },
		{ label: 'Avg', align: 'right' }
	];
	const modelColumns: Column[] = [
		{ label: 'Model' },
		{ label: 'Requests', align: 'right' },
		{ label: 'Error rate', align: 'right' },
		{ label: '5xx', align: 'right' },
		{ label: 'Captured', align: 'right' },
		{ label: 'Avg', align: 'right' }
	];
	const upstreamColumns: Column[] = [
		{ label: 'Upstream' },
		{ label: 'Requests', align: 'right' },
		{ label: 'Error rate', align: 'right' },
		{ label: '2xx / 4xx / 5xx', align: 'right' },
		{ label: 'p95', align: 'right' },
		{ label: 'Avg', align: 'right' }
	];

	function errorRateTone(rate: number): 'danger' | 'warning' | 'neutral' {
		if (rate >= 0.25) return 'danger';
		if (rate >= 0.05) return 'warning';
		return 'neutral';
	}
</script>

<svelte:head><title>Analytics · llmtrace</title></svelte:head>

<PageHeader title="Analytics" description="Usage, latency, and error breakdowns across the selected window.">
	{#snippet actions()}
		<TimeWindowSelect value={hours} onChange={(h) => setParam('hours', String(h), String(DEFAULT_HOURS))} />
	{/snippet}
</PageHeader>

<div class="mb-4">
	<Tabs {tabs} active={tab} onChange={(id) => setParam('tab', id, DEFAULT_TAB)} />
</div>

{#if tab === 'summary'}
	<div class="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
		<StatCard label="Requests" value={formatNumber(summaryTotals?.request_count ?? 0)} icon="list" loading={summary.loading} />
		<StatCard
			label="Errors"
			value={formatNumber(summaryTotals?.error_count ?? 0)}
			icon="alert"
			tone={summaryTotals && summaryTotals.error_count > 0 ? 'danger' : 'default'}
			loading={summary.loading}
		/>
		<StatCard label="Bytes in" value={formatBytes(summaryTotals?.bytes_in ?? 0)} icon="download" loading={summary.loading} />
		<StatCard label="Bytes out" value={formatBytes(summaryTotals?.bytes_out ?? 0)} icon="upload" loading={summary.loading} />
		<StatCard label="Avg duration" value={formatMs(summaryTotals?.avg_duration_ms)} icon="activity" loading={summary.loading} />
		<StatCard label="Avg TTFT" value={formatMs(summaryTotals?.avg_ttft_ms)} icon="activity" loading={summary.loading} />
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
		<Card title="Top models">
			{#if (summary.data?.top_models.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No traffic in this window.</p>
			{:else}
				<div class="flex flex-col gap-3">
					{#each summary.data?.top_models ?? [] as model (model.name)}
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
		<Card title="Top upstreams">
			{#if (summary.data?.top_upstreams.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No traffic in this window.</p>
			{:else}
				<div class="flex flex-col gap-3">
					{#each summary.data?.top_upstreams ?? [] as up (up.name)}
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

	<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
		<Card title="Status classes">
			{#if (summary.data?.status_classes.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No data.</p>
			{:else}
				<div class="flex flex-wrap gap-2">
					{#each summary.data?.status_classes ?? [] as sc (sc.name)}
						<Badge tone="neutral">{sc.name}: {formatNumber(sc.request_count)}</Badge>
					{/each}
				</div>
			{/if}
		</Card>
		<Card title="Request kinds">
			{#if (summary.data?.request_kinds.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No data.</p>
			{:else}
				<div class="flex flex-wrap gap-2">
					{#each summary.data?.request_kinds ?? [] as rk (rk.name)}
						<Badge tone="info">{rk.name}: {formatNumber(rk.request_count)}</Badge>
					{/each}
				</div>
			{/if}
		</Card>
	</div>
{:else if tab === 'timeseries'}
	<div class="mb-3 flex items-center gap-2">
		<span class="text-fg-muted text-xs">Bucket</span>
		<div class="inline-flex overflow-hidden rounded-lg border" style="border-color: var(--color-border);">
			{#each buckets as b, i (b.id)}
				<button
					type="button"
					class="px-3 py-1.5 text-xs font-medium"
					style={bucket === b.id ? 'background-color: var(--color-brand); color: var(--color-brand-fg);' : 'color: var(--color-fg);'}
					class:border-l={i > 0}
					onclick={() => setParam('bucket', b.id, DEFAULT_BUCKET)}
				>{b.label}</button>
			{/each}
		</div>
	</div>
	<Card title="Requests & errors">
		{#if timeseries.loading}
			<div class="h-64 animate-pulse rounded" style="background-color: var(--color-surface-muted);"></div>
		{:else if timeseries.error}
			<p class="text-sm" style="color: var(--color-danger);">{timeseries.error}</p>
		{:else}
			<Chart type="line" data={chartData} options={chartOptions} height="16rem" />
		{/if}
	</Card>
	<div class="mt-4">
		<Card title="Latency trend (avg duration & TTFT)">
			{#if timeseries.loading}
				<div class="h-64 animate-pulse rounded" style="background-color: var(--color-surface-muted);"></div>
			{:else}
				<Chart type="line" data={latencyChartData} options={chartOptions} height="16rem" />
			{/if}
		</Card>
	</div>
{:else if tab === 'latency'}
	{@const totals = latency.data?.totals}
	<div class="grid grid-cols-2 gap-3 md:grid-cols-4">
		<StatCard label="p50 duration" value={formatMs(totals?.p50_duration_ms)} icon="activity" loading={latency.loading} />
		<StatCard label="p90 duration" value={formatMs(totals?.p90_duration_ms)} icon="activity" loading={latency.loading} />
		<StatCard label="p95 duration" value={formatMs(totals?.p95_duration_ms)} icon="activity" loading={latency.loading} />
		<StatCard label="p99 duration" value={formatMs(totals?.p99_duration_ms)} icon="activity" loading={latency.loading} />
		<StatCard label="p50 TTFT" value={formatMs(totals?.p50_ttft_ms)} icon="clock" loading={latency.loading} />
		<StatCard label="p90 TTFT" value={formatMs(totals?.p90_ttft_ms)} icon="clock" loading={latency.loading} />
		<StatCard label="p95 TTFT" value={formatMs(totals?.p95_ttft_ms)} icon="clock" loading={latency.loading} />
		<StatCard label="p99 TTFT" value={formatMs(totals?.p99_ttft_ms)} icon="clock" loading={latency.loading} />
	</div>

	{#snippet latencyTable(title: string, rows: (LatencyMetric & { name: string })[])}
		<div class="mt-4">
			<h2 class="mb-2 text-sm font-semibold">{title}</h2>
			<DataTable columns={latencyColumns} {rows} loading={latency.loading} error={latency.error} emptyTitle="No data" onRetry={() => latency.load()}>
				{#snippet row(item: LatencyMetric & { name: string })}
					<td class="px-3 py-2">{item.name}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.p50_duration_ms)}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.p90_duration_ms)}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.p95_duration_ms)}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.p99_duration_ms)}</td>
					<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.max_duration_ms)}</td>
				{/snippet}
			</DataTable>
		</div>
	{/snippet}

	{@render latencyTable('By model', latency.data?.top_models ?? [])}
	{@render latencyTable('By upstream', latency.data?.top_upstreams ?? [])}
	{@render latencyTable('By request kind', latency.data?.request_kinds ?? [])}
{:else if tab === 'api-keys'}
	<DataTable
		columns={apiKeyColumns}
		rows={apiKeys.data?.items ?? []}
		loading={apiKeys.loading}
		error={apiKeys.error}
		emptyTitle="No API key activity"
		onRetry={() => apiKeys.load()}
	>
		{#snippet row(item: ApiKeyUsageItem)}
			<td class="px-3 py-2 font-mono text-xs">
				<a class="hover:underline" style="color: var(--color-brand);" href={`${base}/requests?api_key_hash=${encodeURIComponent(item.api_key_hash)}`}>
					{truncateMiddle(item.api_key_hash, 24)}
				</a>
			</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.error_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.session_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatBytes(item.captured_bytes)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.avg_duration_ms)}</td>
		{/snippet}
	</DataTable>
{:else if tab === 'users'}
	<DataTable
		columns={userColumns}
		rows={users.data?.items ?? []}
		loading={users.loading}
		error={users.error}
		emptyTitle="No user activity"
		onRetry={() => users.load()}
	>
		{#snippet row(item: UserUsageItem)}
			<td class="px-3 py-2">{item.user_name ?? item.user_id ?? '—'}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
			<td class="px-3 py-2 text-right"><Badge tone={errorRateTone(item.error_rate)}>{formatPercent(item.error_rate)}</Badge></td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.api_key_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.session_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.avg_duration_ms)}</td>
		{/snippet}
	</DataTable>
{:else if tab === 'models'}
	<DataTable
		columns={modelColumns}
		rows={models.data?.items ?? []}
		loading={models.loading}
		error={models.error}
		emptyTitle="No model activity"
		onRetry={() => models.load()}
	>
		{#snippet row(item: ModelUsageItem)}
			<td class="px-3 py-2">
				<a class="hover:underline" style="color: var(--color-brand);" href={`${base}/requests?model=${encodeURIComponent(item.model)}`}>{item.model}</a>
			</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
			<td class="px-3 py-2 text-right"><Badge tone={errorRateTone(item.error_rate)}>{formatPercent(item.error_rate)}</Badge></td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.http_5xx_count)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatBytes(item.captured_bytes)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.avg_duration_ms)}</td>
		{/snippet}
	</DataTable>
{:else if tab === 'upstreams'}
	<DataTable
		columns={upstreamColumns}
		rows={upstreams.data?.items ?? []}
		loading={upstreams.loading}
		error={upstreams.error}
		emptyTitle="No upstream activity"
		onRetry={() => upstreams.load()}
	>
		{#snippet row(item: UpstreamHealthItem)}
			<td class="px-3 py-2">
				{#if item.upstream_host}
					<a class="hover:underline" style="color: var(--color-brand);" href={`${base}/requests?upstream_host=${encodeURIComponent(item.upstream_host)}`}>{item.upstream_host}</a>
				{:else}—{/if}
			</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatNumber(item.request_count)}</td>
			<td class="px-3 py-2 text-right"><Badge tone={errorRateTone(item.error_rate)}>{formatPercent(item.error_rate)}</Badge></td>
			<td class="px-3 py-2 text-right tabular-nums text-xs">
				{formatNumber(item.http_2xx_count)} / {formatNumber(item.http_4xx_count)} / {formatNumber(item.http_5xx_count)}
			</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.p95_duration_ms)}</td>
			<td class="px-3 py-2 text-right tabular-nums">{formatMs(item.avg_duration_ms)}</td>
		{/snippet}
	</DataTable>
{:else if tab === 'errors'}
	{@const t = errors.data?.totals}
	<div class="grid grid-cols-2 gap-3 md:grid-cols-4">
		<StatCard label="Errors" value={formatNumber(t?.error_count ?? 0)} icon="alert" tone={t && t.error_count > 0 ? 'danger' : 'default'} loading={errors.loading} />
		<StatCard label="Proxy errors" value={formatNumber(t?.proxy_error_count ?? 0)} icon="alert" loading={errors.loading} />
		<StatCard label="HTTP 5xx" value={formatNumber(t?.http_5xx_count ?? 0)} icon="alert" loading={errors.loading} />
		<StatCard label="Affected sessions" value={formatNumber(t?.affected_sessions ?? 0)} icon="messages" loading={errors.loading} />
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
		<Card title="Error sources">
			{#if (errors.data?.sources.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No errors in this window.</p>
			{:else}
				<div class="flex flex-col gap-2 text-sm">
					{#each errors.data?.sources ?? [] as src (src.name)}
						<div class="flex items-center justify-between">
							<span>{src.name}</span>
							<span class="tabular-nums">{formatNumber(src.error_count)}</span>
						</div>
					{/each}
				</div>
			{/if}
		</Card>
		<Card title="Status classes">
			{#if (errors.data?.status_classes.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No errors in this window.</p>
			{:else}
				<div class="flex flex-col gap-2 text-sm">
					{#each errors.data?.status_classes ?? [] as sc (sc.name)}
						<div class="flex items-center justify-between">
							<span>{sc.name}</span>
							<span class="tabular-nums">{formatNumber(sc.error_count)}</span>
						</div>
					{/each}
				</div>
			{/if}
		</Card>
	</div>

	<div class="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
		<Card title="Top failing models">
			{#if (errors.data?.top_models.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No data.</p>
			{:else}
				<div class="flex flex-col gap-2 text-sm">
					{#each errors.data?.top_models ?? [] as m (m.name)}
						<a class="flex items-center justify-between hover:underline" href={`${base}/requests?has_error=true&model=${encodeURIComponent(m.name)}`}>
							<span>{m.name}</span>
							<span class="tabular-nums">{formatNumber(m.error_count)}</span>
						</a>
					{/each}
				</div>
			{/if}
		</Card>
		<Card title="Top failing upstreams">
			{#if (errors.data?.top_upstreams.length ?? 0) === 0}
				<p class="text-fg-muted text-sm">No data.</p>
			{:else}
				<div class="flex flex-col gap-2 text-sm">
					{#each errors.data?.top_upstreams ?? [] as u (u.name)}
						<a class="flex items-center justify-between hover:underline" href={`${base}/requests?has_error=true&upstream_host=${encodeURIComponent(u.name)}`}>
							<span>{u.name}</span>
							<span class="tabular-nums">{formatNumber(u.error_count)}</span>
						</a>
					{/each}
				</div>
			{/if}
		</Card>
	</div>
{/if}
