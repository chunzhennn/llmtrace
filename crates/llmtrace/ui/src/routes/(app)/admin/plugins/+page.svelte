<script lang="ts">
	import { onMount } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import StatCard from '$lib/components/StatCard.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { Resource } from '$lib/utils/resource.svelte';
	import * as admin from '$lib/api/endpoints/admin';
	import type { PluginsResponse } from '$lib/api/types';
	import { formatNumber } from '$lib/utils/format';

	const plugins = new Resource<PluginsResponse>((signal) => admin.plugins(signal));
	onMount(() => plugins.load());
</script>

<svelte:head><title>Plugins · llmtrace</title></svelte:head>

<PageHeader title="Plugins" description="WASM trace-enrichment plugins and their load status.">
	{#snippet actions()}
		<button type="button" class="btn" onclick={() => plugins.load()}><Icon name="refresh" size={16} /> Refresh</button>
	{/snippet}
</PageHeader>

{#if plugins.loading && !plugins.data}
	<Card><Spinner label="Loading plugins…" /></Card>
{:else if plugins.error}
	<Card><ErrorState message={plugins.error} onRetry={() => plugins.load()} /></Card>
{:else if plugins.data}
	<div class="grid grid-cols-3 gap-3">
		<StatCard label="Configured" value={formatNumber(plugins.data.configured_count)} icon="box" />
		<StatCard label="Loaded" value={formatNumber(plugins.data.loaded_count)} icon="check" tone="success" />
		<StatCard label="Failed" value={formatNumber(plugins.data.failed_count)} icon="alert" tone={plugins.data.failed_count > 0 ? 'danger' : 'default'} />
	</div>

	{#if plugins.data.items.length === 0}
		<Card class="mt-4"><EmptyState icon="box" title="No plugins" message="No WASM plugins are configured." /></Card>
	{:else}
		<div class="mt-4 grid grid-cols-1 gap-4 md:grid-cols-2">
			{#each plugins.data.items as plugin (plugin.name)}
				<Card>
					<div class="flex items-start justify-between gap-3">
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<Icon name="box" size={16} />
								<span class="truncate font-semibold">{plugin.name}</span>
							</div>
							<div class="mt-2 flex flex-wrap gap-1">
								{#if plugin.hooks.length === 0}
									<span class="text-fg-muted text-xs">No hooks</span>
								{:else}
									{#each plugin.hooks as hook (hook)}
										<Badge tone="neutral">{hook}</Badge>
									{/each}
								{/if}
							</div>
						</div>
						{#if plugin.loaded}
							<Badge tone="success">loaded</Badge>
						{:else}
							<Badge tone="danger">failed</Badge>
						{/if}
					</div>
					{#if plugin.error}
						<p class="mt-3 rounded-lg p-2 text-xs" style="background-color: var(--color-surface-muted); color: var(--color-danger);">
							{plugin.error}
						</p>
					{/if}
				</Card>
			{/each}
		</div>
	{/if}
{/if}
