<script lang="ts">
	import { onMount } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import Field from '$lib/components/Field.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import { createResource } from '$lib/utils/resource.svelte';
	import { ApiError } from '$lib/api/client';
	import * as queryApi from '$lib/api/endpoints/query';
	import { exportJsonl } from '$lib/api/download';
	import { toasts } from '$lib/state/toast.svelte';
	import type {
		QuerySchema,
		DatasetSpec,
		FieldSpec,
		QueryOp,
		SortDirection,
		StructuredQueryRequest,
		StructuredQueryResult
	} from '$lib/api/types';
	import { coerceFilterValue, valueInputKind, opNeedsValue, displayCell } from '$lib/utils/query';

	interface FilterRow {
		id: number;
		field: string;
		op: QueryOp;
		value: string;
		custom: boolean;
	}
	interface OrderRow {
		id: number;
		field: string;
		direction: SortDirection;
	}

	let rowSeq = 0;
	const nextId = () => ++rowSeq;

	const schema = createResource<QuerySchema>((signal) => queryApi.querySchema(signal));

	let dataset = $state('');
	let selectedFields = $state<string[]>([]);
	let filters = $state<FilterRow[]>([]);
	let orderBy = $state<OrderRow[]>([]);
	let limit = $state(100);

	let running = $state(false);
	let result = $state<StructuredQueryResult | undefined>();
	let runError = $state<string | null>(null);

	const currentDataset = $derived<DatasetSpec | undefined>(
		schema.data?.datasets.find((d) => d.name === dataset)
	);
	const filterableFields = $derived<FieldSpec[]>(
		(currentDataset?.fields ?? []).filter((f) => f.filter_kind && f.operators.length > 0)
	);
	const supportsPluginMetadata = $derived(schema.data?.plugin_metadata.dataset === dataset);
	const maxFields = $derived(schema.data?.limits.max_fields ?? 50);
	const maxLimit = $derived(schema.data?.limits.max_limit ?? 500);

	function applyDatasetDefaults(spec: DatasetSpec) {
		dataset = spec.name;
		selectedFields = [...spec.default_fields];
		filters = [];
		orderBy = spec.default_order.map((o) => ({
			id: nextId(),
			field: o.field,
			direction: o.direction ?? 'desc'
		}));
		result = undefined;
		runError = null;
	}

	onMount(async () => {
		await schema.load();
		const data = schema.data;
		if (data) {
			const preferred =
				data.datasets.find((d) => d.name === 'requests') ?? data.datasets[0];
			if (preferred) applyDatasetDefaults(preferred);
		}
	});

	function onDatasetChange(name: string) {
		const spec = schema.data?.datasets.find((d) => d.name === name);
		if (spec) applyDatasetDefaults(spec);
	}

	function toggleField(name: string) {
		if (selectedFields.includes(name)) {
			selectedFields = selectedFields.filter((f) => f !== name);
		} else {
			selectedFields = [...selectedFields, name];
		}
	}

	function selectAllFields() {
		selectedFields = (currentDataset?.fields ?? []).map((f) => f.name);
	}
	function clearFields() {
		selectedFields = [];
	}

	function fieldSpec(name: string): FieldSpec | undefined {
		return currentDataset?.fields.find((f) => f.name === name);
	}

	function rowKind(row: FilterRow): string | null {
		if (row.custom) return schema.data?.plugin_metadata.filter_kind ?? 'json_path';
		return fieldSpec(row.field)?.filter_kind ?? null;
	}
	function rowOperators(row: FilterRow): QueryOp[] {
		if (row.custom) return schema.data?.plugin_metadata.operators ?? ['eq', 'ne', 'contains'];
		return fieldSpec(row.field)?.operators ?? [];
	}

	function addFilter() {
		const first = filterableFields[0];
		filters = [
			...filters,
			{
				id: nextId(),
				field: first?.name ?? '',
				op: (first?.operators[0] ?? 'eq') as QueryOp,
				value: '',
				custom: false
			}
		];
	}
	function addPluginFilter() {
		const ops = schema.data?.plugin_metadata.operators ?? ['eq'];
		filters = [
			...filters,
			{
				id: nextId(),
				field: schema.data?.plugin_metadata.field_prefix ?? 'plugin_metadata.',
				op: (ops[0] ?? 'eq') as QueryOp,
				value: '',
				custom: true
			}
		];
	}
	function removeFilter(id: number) {
		filters = filters.filter((f) => f.id !== id);
	}
	function onFilterFieldChange(row: FilterRow, name: string) {
		const spec = fieldSpec(name);
		row.field = name;
		if (spec && !spec.operators.includes(row.op)) {
			row.op = (spec.operators[0] ?? 'eq') as QueryOp;
		}
	}

	function addOrder() {
		const first = currentDataset?.fields[0];
		orderBy = [...orderBy, { id: nextId(), field: first?.name ?? '', direction: 'desc' }];
	}
	function removeOrder(id: number) {
		orderBy = orderBy.filter((o) => o.id !== id);
	}

	function buildRequest(): { request?: StructuredQueryRequest; error?: string } {
		const outFilters = [];
		for (const row of filters) {
			if (!row.field.trim()) return { error: 'Every filter needs a field.' };
			if (!opNeedsValue(row.op)) {
				outFilters.push({ field: row.field.trim(), op: row.op });
				continue;
			}
			const coerced = coerceFilterValue(rowKind(row), row.value);
			if (!coerced.ok) return { error: `Filter "${row.field}": ${coerced.error}` };
			outFilters.push({ field: row.field.trim(), op: row.op, value: coerced.value });
		}
		const outOrder = orderBy
			.filter((o) => o.field.trim())
			.map((o) => ({ field: o.field.trim(), direction: o.direction }));
		const request: StructuredQueryRequest = {
			dataset,
			fields: selectedFields.length ? selectedFields : undefined,
			filters: outFilters.length ? outFilters : undefined,
			order_by: outOrder.length ? outOrder : undefined,
			limit: limit && limit > 0 ? limit : undefined
		};
		return { request };
	}

	async function run() {
		const { request, error } = buildRequest();
		if (error || !request) {
			runError = error ?? 'Invalid query.';
			return;
		}
		running = true;
		runError = null;
		try {
			result = await queryApi.runQuery(request);
		} catch (err) {
			runError = err instanceof ApiError || err instanceof Error ? err.message : 'Query failed.';
		} finally {
			running = false;
		}
	}

	async function exportResults() {
		const { request, error } = buildRequest();
		if (error || !request) {
			toasts.error(error ?? 'Invalid query.');
			return;
		}
		try {
			const rows = await exportJsonl.query(request);
			toasts.success(`Exported ${rows} row${rows === 1 ? '' : 's'}.`);
		} catch (err) {
			toasts.error(err instanceof Error ? err.message : 'Export failed.');
		}
	}

</script>

<svelte:head><title>Query · llmtrace</title></svelte:head>

<PageHeader title="Query" description="Build structured queries over allowlisted datasets and fields." />

{#if schema.loading}
	<Card><Spinner label="Loading schema…" /></Card>
{:else if schema.error}
	<Card><ErrorState message={schema.error} onRetry={() => schema.load()} /></Card>
{:else if schema.data}
	<div class="grid grid-cols-1 gap-4 xl:grid-cols-[22rem_minmax(0,1fr)]">
		<div class="flex flex-col gap-4">
			<Card title="Dataset & fields">
				<div class="flex flex-col gap-4">
					<Field label="Dataset">
						<select class="input" value={dataset} onchange={(e) => onDatasetChange(e.currentTarget.value)}>
							{#each schema.data.datasets as ds (ds.name)}
								<option value={ds.name}>{ds.name}</option>
							{/each}
						</select>
					</Field>

					<div>
						<div class="mb-1 flex items-center justify-between">
							<span class="label">Fields ({selectedFields.length})</span>
							<span class="flex gap-2 text-xs">
								<button type="button" class="hover:underline" style="color: var(--color-brand);" onclick={selectAllFields}>All</button>
								<button type="button" class="hover:underline" style="color: var(--color-brand);" onclick={clearFields}>Clear</button>
							</span>
						</div>
						<div class="max-h-64 overflow-y-auto rounded-lg border p-2" style="border-color: var(--color-border);">
							{#each currentDataset?.fields ?? [] as f (f.name)}
								<label class="flex cursor-pointer items-center gap-2 rounded px-1 py-0.5 text-sm hover:bg-[var(--color-surface-muted)]">
									<input type="checkbox" checked={selectedFields.includes(f.name)} onchange={() => toggleField(f.name)} />
									<span class="font-mono text-xs">{f.name}</span>
									{#if !f.filter_kind}<span class="text-fg-muted text-[10px]">(display)</span>{/if}
								</label>
							{/each}
						</div>
						<p class="text-fg-muted mt-1 text-xs">Leave empty to use dataset defaults. Max {maxFields}.</p>
					</div>

					<Field label="Limit" hint={`Max ${maxLimit}`}>
						<input class="input" type="number" min="1" max={maxLimit} bind:value={limit} />
					</Field>
				</div>
			</Card>

			<Card title="Filters">
				{#snippet actions()}
					<button type="button" class="btn !px-2 !py-1 text-xs" onclick={addFilter}>
						<Icon name="plus" size={14} /> Filter
					</button>
				{/snippet}
				<div class="flex flex-col gap-3">
					{#if filters.length === 0}
						<p class="text-fg-muted text-sm">No filters. All rows match.</p>
					{/if}
					{#each filters as row (row.id)}
						<div class="rounded-lg border p-2" style="border-color: var(--color-border);">
							<div class="flex items-center gap-2">
								{#if row.custom}
									<input
										class="input flex-1 font-mono text-xs"
										type="text"
										aria-label="Plugin metadata path"
										placeholder="plugin_metadata.name.path"
										bind:value={row.field}
									/>
								{:else}
									<select class="input min-w-0 flex-1" aria-label="Filter field" value={row.field} onchange={(e) => onFilterFieldChange(row, e.currentTarget.value)}>
										{#each filterableFields as f (f.name)}
											<option value={f.name}>{f.name}</option>
										{/each}
									</select>
								{/if}
								<button type="button" class="btn !px-2 !py-1" aria-label="Remove filter" onclick={() => removeFilter(row.id)}>
									<Icon name="trash" size={14} />
								</button>
							</div>
							<div class="mt-2 grid grid-cols-2 items-center gap-2">
								<select class="input min-w-0" aria-label="Filter operator" bind:value={row.op}>
									{#each rowOperators(row) as op (op)}
										<option value={op}>{op}</option>
									{/each}
								</select>
								{#if valueInputKind(rowKind(row), row.op) === 'bool'}
									<select class="input min-w-0" aria-label="Filter value" bind:value={row.value}>
										<option value="true">true</option>
										<option value="false">false</option>
									</select>
								{:else if valueInputKind(rowKind(row), row.op) === 'int'}
									<input class="input min-w-0" aria-label="Filter value" type="number" bind:value={row.value} />
								{:else if valueInputKind(rowKind(row), row.op) === 'timestamp'}
									<input class="input min-w-0" aria-label="Filter value" type="datetime-local" bind:value={row.value} />
								{:else if valueInputKind(rowKind(row), row.op) === 'text'}
									<input class="input min-w-0" aria-label="Filter value" type="text" placeholder="value" bind:value={row.value} />
								{/if}
							</div>
							{#if row.custom}<p class="text-fg-muted mt-1 text-[10px]">Plugin metadata path · json_path</p>{/if}
						</div>
					{/each}
					{#if supportsPluginMetadata}
						<button type="button" class="btn !px-2 !py-1 self-start text-xs" onclick={addPluginFilter}>
							<Icon name="plus" size={14} /> Plugin metadata filter
						</button>
					{/if}
				</div>
			</Card>

			<Card title="Sort">
				{#snippet actions()}
					<button type="button" class="btn !px-2 !py-1 text-xs" onclick={addOrder}>
						<Icon name="plus" size={14} /> Sort
					</button>
				{/snippet}
				<div class="flex flex-col gap-2">
					{#if orderBy.length === 0}
						<p class="text-fg-muted text-sm">No sort keys.</p>
					{/if}
					{#each orderBy as row (row.id)}
						<div class="flex items-center gap-2">
							<select class="input min-w-0 flex-1" aria-label="Sort field" bind:value={row.field}>
								{#each currentDataset?.fields ?? [] as f (f.name)}
									<option value={f.name}>{f.name}</option>
								{/each}
							</select>
							<select class="input !w-auto shrink-0" aria-label="Sort direction" bind:value={row.direction}>
								{#each schema.data.sort_directions as dir (dir)}
									<option value={dir}>{dir}</option>
								{/each}
							</select>
							<button type="button" class="btn !px-2 !py-1" aria-label="Remove sort" onclick={() => removeOrder(row.id)}>
								<Icon name="trash" size={14} />
							</button>
						</div>
					{/each}
				</div>
			</Card>

			<div class="flex gap-2">
				<button type="button" class="btn btn-brand flex-1" onclick={run} disabled={running}>
					{#if running}<Icon name="refresh" size={16} class="animate-spin" /> Running…{:else}<Icon name="search" size={16} /> Run query{/if}
				</button>
				<button type="button" class="btn" onclick={exportResults} disabled={running}>
					<Icon name="download" size={16} /> Export
				</button>
			</div>
		</div>

		<div class="min-w-0">
			{#if runError}
				<div class="card mb-3 p-3 text-sm" style="border-color: var(--color-danger); color: var(--color-danger);">
					{runError}
				</div>
			{/if}
			<Card
				title="Results"
				subtitle={result ? `${result.rows.length} row${result.rows.length === 1 ? '' : 's'} · limit ${result.limit}` : undefined}
				bodyClass="p-0"
			>
				{#snippet actions()}
					{#if result}<Badge tone="neutral">{result.dataset}</Badge>{/if}
				{/snippet}
				{#if running}
					<div class="p-6"><Spinner label="Running query…" /></div>
				{:else if !result}
					<div class="p-6"><EmptyState icon="chart" title="No results yet" message="Configure the query and run it." /></div>
				{:else if result.rows.length === 0}
					<div class="p-6"><EmptyState icon="inbox" title="No rows" message="The query returned no rows." /></div>
				{:else}
					<div class="overflow-x-auto">
						<table class="w-full text-left text-sm">
							<thead style="border-bottom: 1px solid var(--color-border);">
								<tr>
									{#each result.fields as col (col)}
										<th class="text-fg-muted px-3 py-2 font-medium whitespace-nowrap">{col}</th>
									{/each}
								</tr>
							</thead>
							<tbody>
								{#each result.rows as r, i (i)}
									<tr style="border-bottom: 1px solid var(--color-border);">
										{#each result.fields as col (col)}
											<td class="max-w-[24rem] truncate px-3 py-2 font-mono text-xs" title={displayCell(r[col])}>
												{#if r[col] === null || r[col] === undefined}
													<span class="text-fg-muted">—</span>
												{:else}
													{displayCell(r[col])}
												{/if}
											</td>
										{/each}
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</Card>
		</div>
	</div>
{/if}
