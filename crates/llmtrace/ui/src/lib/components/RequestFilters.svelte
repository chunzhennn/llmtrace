<script lang="ts">
	import { untrack } from 'svelte';
	import Field from './Field.svelte';
	import Icon from './Icon.svelte';
	import type { RequestFacets } from '$lib/api/types';
	import { REQUEST_KINDS, STATUS_CLASSES } from '$lib/api/types';

	const DEFAULT_LIMIT = 50;

	interface Props {
		params: URLSearchParams;
		facets: RequestFacets | undefined;
		activeFilterCount: number;
		onApply: (search: string) => void;
		onReset: () => void;
	}

	let { params, facets, activeFilterCount, onApply, onReset }: Props = $props();

	function isoToLocalInput(value: string): string {
		if (!value) return '';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return '';
		const local = new Date(date.getTime() - date.getTimezoneOffset() * 60000);
		return local.toISOString().slice(0, 16);
	}

	function localInputToIso(value: string): string {
		if (!value) return '';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return '';
		return date.toISOString();
	}

	// Seed draft values once from the URL params. This component is recreated via
	// {#key page.url.search} in the parent, so reading the initial snapshot here is
	// intentional; untrack makes that explicit and avoids reactive-capture warnings.
	const seed = untrack(() => ({
		q: params.get('q') ?? '',
		status: params.get('status') ?? '',
		statusClass: params.get('status_class') ?? '',
		hasError: params.get('has_error') ?? '',
		upstreamHost: params.get('upstream_host') ?? '',
		model: params.get('model') ?? '',
		requestKind: params.get('request_kind') ?? '',
		sessionId: params.get('session_id') ?? '',
		apiKeyHash: params.get('api_key_hash') ?? '',
		since: isoToLocalInput(params.get('since') ?? ''),
		until: isoToLocalInput(params.get('until') ?? ''),
		minDuration: params.get('min_duration_ms') ?? '',
		maxDuration: params.get('max_duration_ms') ?? '',
		limit: params.get('limit') ?? String(DEFAULT_LIMIT),
		advanced: Boolean(
			params.get('status') ||
				params.get('session_id') ||
				params.get('api_key_hash') ||
				params.get('since') ||
				params.get('until') ||
				params.get('min_duration_ms') ||
				params.get('max_duration_ms')
		)
	}));

	let q = $state(seed.q);
	let status = $state(seed.status);
	let statusClass = $state(seed.statusClass);
	let hasError = $state(seed.hasError);
	let upstreamHost = $state(seed.upstreamHost);
	let model = $state(seed.model);
	let requestKind = $state(seed.requestKind);
	let sessionId = $state(seed.sessionId);
	let apiKeyHash = $state(seed.apiKeyHash);
	let since = $state(seed.since);
	let until = $state(seed.until);
	let minDuration = $state(seed.minDuration);
	let maxDuration = $state(seed.maxDuration);
	let limit = $state(seed.limit);
	let showAdvanced = $state(seed.advanced);

	function submit(event: SubmitEvent) {
		event.preventDefault();
		const next = new URLSearchParams();
		const set = (key: string, value: string) => {
			if (value.trim() !== '') next.set(key, value.trim());
		};
		set('q', q);
		set('status', status);
		set('status_class', statusClass);
		set('has_error', hasError);
		set('upstream_host', upstreamHost);
		set('model', model);
		set('request_kind', requestKind);
		set('session_id', sessionId);
		set('api_key_hash', apiKeyHash);
		set('since', localInputToIso(since));
		set('until', localInputToIso(until));
		set('min_duration_ms', minDuration);
		set('max_duration_ms', maxDuration);
		if (limit && limit !== String(DEFAULT_LIMIT)) set('limit', limit);
		onApply(next.toString());
	}
</script>

<form onsubmit={submit} class="card mb-4 flex flex-col gap-3 p-3">
	<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
		<Field label="Search" class="lg:col-span-2">
			<input class="input" type="text" placeholder="URI, model, host, session…" bind:value={q} />
		</Field>
		<Field label="Model">
			<select class="input" bind:value={model}>
				<option value="">All models</option>
				{#each facets?.facets.models ?? [] as m (m.value)}
					<option value={m.value}>{m.value} ({m.request_count})</option>
				{/each}
			</select>
		</Field>
		<Field label="Upstream host">
			<select class="input" bind:value={upstreamHost}>
				<option value="">All upstreams</option>
				{#each facets?.facets.upstream_hosts ?? [] as h (h.value)}
					<option value={h.value}>{h.value} ({h.request_count})</option>
				{/each}
			</select>
		</Field>
		<Field label="Request kind">
			<select class="input" bind:value={requestKind}>
				<option value="">All kinds</option>
				{#each REQUEST_KINDS as kind (kind)}
					<option value={kind}>{kind}</option>
				{/each}
			</select>
		</Field>
		<Field label="Status class">
			<select class="input" bind:value={statusClass}>
				<option value="">All statuses</option>
				{#each STATUS_CLASSES as cls (cls)}
					<option value={cls}>{cls}</option>
				{/each}
			</select>
		</Field>
		<Field label="Errors only">
			<select class="input" bind:value={hasError}>
				<option value="">Any</option>
				<option value="true">Errors only</option>
				<option value="false">Success only</option>
			</select>
		</Field>
		<Field label="Rows">
			<select class="input" bind:value={limit}>
				<option value="25">25</option>
				<option value="50">50</option>
				<option value="100">100</option>
				<option value="200">200</option>
			</select>
		</Field>
	</div>

	{#if showAdvanced}
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
			<Field label="Exact status">
				<input class="input" type="number" placeholder="e.g. 200" bind:value={status} />
			</Field>
			<Field label="Session ID">
				<input class="input" type="text" placeholder="UUID" bind:value={sessionId} />
			</Field>
			<Field label="API key hash">
				<input class="input" type="text" placeholder="sha256:…" bind:value={apiKeyHash} />
			</Field>
			<div></div>
			<Field label="Since">
				<input class="input" type="datetime-local" bind:value={since} />
			</Field>
			<Field label="Until">
				<input class="input" type="datetime-local" bind:value={until} />
			</Field>
			<Field label="Min duration (ms)">
				<input class="input" type="number" min="0" bind:value={minDuration} />
			</Field>
			<Field label="Max duration (ms)">
				<input class="input" type="number" min="0" bind:value={maxDuration} />
			</Field>
		</div>
	{/if}

	<div class="flex flex-wrap items-center gap-2">
		<button type="submit" class="btn btn-brand">
			<Icon name="filter" size={16} /> Apply filters
		</button>
		<button type="button" class="btn" onclick={() => (showAdvanced = !showAdvanced)}>
			{showAdvanced ? 'Fewer filters' : 'More filters'}
		</button>
		{#if activeFilterCount > 0}
			<button type="button" class="btn" onclick={onReset}>
				<Icon name="x" size={16} /> Reset ({activeFilterCount})
			</button>
		{/if}
	</div>
</form>
