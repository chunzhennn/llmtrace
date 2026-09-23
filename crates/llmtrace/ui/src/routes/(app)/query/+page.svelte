<script lang="ts">
	import ChartNoAxesColumnIcon from '@lucide/svelte/icons/chart-no-axes-column';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import Columns3Icon from '@lucide/svelte/icons/columns-3';
	import DatabaseIcon from '@lucide/svelte/icons/database';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import InboxIcon from '@lucide/svelte/icons/inbox';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';
	import SearchIcon from '@lucide/svelte/icons/search';
	import XIcon from '@lucide/svelte/icons/x';
	import { onMount } from 'svelte';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import Card from '$lib/components/Card.svelte';
	import Spinner from '$lib/components/Spinner.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import ErrorState from '$lib/components/ErrorState.svelte';
	import Badge from '$lib/components/Badge.svelte';
	import Tabs from '$lib/components/Tabs.svelte';
	import QueryCodeEditor from '$lib/components/QueryCodeEditor.svelte';
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
	import { formatQueryText, parseQueryText, queryErrorMessage, queryField } from '$lib/utils/query-text';

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
	const operatorLabels: Record<QueryOp, string> = {
		eq: 'equals', ne: 'does not equal', contains: 'contains',
		gt: 'greater than', gte: 'at least', lt: 'less than', lte: 'at most',
		is_null: 'is missing', is_not_null: 'exists'
	};

	const schema = createResource<QuerySchema>((signal) => queryApi.querySchema(signal));

	let dataset = $state('');
	let selectedFields = $state<string[]>([]);
	let columnSearch = $state('');
	let filters = $state<FilterRow[]>([]);
	let orderBy = $state<OrderRow[]>([]);
	let limit = $state(100);
	let mode = $state<'builder' | 'code'>('builder');
	let code = $state('');
	let savedCodeDraft = $state<string | null>(null);
	let builderAtCodeError = '';
	let codeAtEntry = '';
	let modeNotice = $state<string | null>(null);

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
	const maxFilters = $derived(schema.data?.limits.max_filters ?? 32);
	const maxOrder = $derived(schema.data?.limits.max_order_by ?? 8);
	const availableColumns = $derived([
		...(currentDataset?.fields.map((field) => field.name) ?? []),
		...selectedFields.filter((name) => !fieldSpec(name))
	]);
	const matchingColumns = $derived(availableColumns.filter((name) =>
		name.toLowerCase().includes(columnSearch.trim().toLowerCase())
	));

	function applyDatasetDefaults(spec: DatasetSpec) {
		dataset = spec.name;
		selectedFields = [...spec.default_fields];
		columnSearch = '';
		filters = [];
		orderBy = spec.default_order.map((o) => ({
			id: nextId(),
			field: o.field,
			direction: o.direction ?? 'desc'
		}));
		result = undefined;
		runError = null;
	}

	onMount(() => { void schema.load(); });
	$effect(() => {
		const data = schema.data;
		if (data && !dataset) {
			const preferred =
				data.datasets.find((d) => d.name === 'requests') ?? data.datasets[0];
			if (preferred) applyDatasetDefaults(preferred);
		}
	});

	function onDatasetChange(name: string) {
		const spec = schema.data?.datasets.find((d) => d.name === name);
		if (spec) applyDatasetDefaults(spec);
	}

	function clearBuilder() {
		if (!currentDataset || running) return;
		applyDatasetDefaults(currentDataset);
		orderBy = [];
		limit = 100;
		code = '';
		savedCodeDraft = null;
		builderAtCodeError = '';
		codeAtEntry = '';
		modeNotice = null;
	}

	function toggleField(name: string) {
		if (selectedFields.includes(name)) {
			if (selectedFields.length > 1) selectedFields = selectedFields.filter((f) => f !== name);
		} else if (selectedFields.length < maxFields) {
			selectedFields = [...selectedFields, name];
		}
	}

	function selectAllFields() {
		if (availableColumns.length <= maxFields) selectedFields = [...availableColumns];
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

	function addFilter(field: string) {
		if (filters.length >= maxFilters) return;
		const custom = field === '__plugin__';
		const spec = fieldSpec(field);
		filters = [
			...filters,
			{
				id: nextId(),
				field: custom ? (schema.data?.plugin_metadata.field_prefix ?? 'plugin_metadata.') : field,
				op: (custom ? schema.data?.plugin_metadata.operators[0] : spec?.operators[0]) ?? 'eq',
				value: spec?.filter_kind === 'bool' ? 'true' : '',
				custom
			}
		];
	}
	function removeFilter(id: number) {
		filters = filters.filter((f) => f.id !== id);
	}
	function onFilterFieldChange(row: FilterRow, name: string) {
		const spec = fieldSpec(name);
		if (rowKind(row) !== spec?.filter_kind) row.value = spec?.filter_kind === 'bool' ? 'true' : '';
		row.field = name;
		if (spec && !spec.operators.includes(row.op)) {
			row.op = (spec.operators[0] ?? 'eq') as QueryOp;
		}
	}
	function onFilterOperatorChange(row: FilterRow, op: QueryOp) {
		row.op = op;
		if (rowKind(row) === 'bool' && opNeedsValue(op) && !['true', 'false'].includes(row.value)) {
			row.value = 'true';
		}
	}

	function addOrder() {
		if (orderBy.length >= maxOrder) return;
		const first = currentDataset?.fields.find((field) => !orderBy.some((row) => row.field === field.name));
		if (!first) return;
		orderBy = [...orderBy, { id: nextId(), field: first.name, direction: 'desc' }];
	}
	function removeOrder(id: number) {
		orderBy = orderBy.filter((o) => o.id !== id);
	}

	function buildBuilderRequest(): { request?: StructuredQueryRequest; error?: string } {
		if (!Number.isInteger(limit) || limit < 1 || limit > maxLimit) {
			return { error: `Row limit must be a whole number between 1 and ${maxLimit}.` };
		}
		const outFilters = [];
		for (const row of filters) {
			if (!row.field.trim()) return { error: 'Every filter needs a field.' };
			if (!opNeedsValue(row.op)) {
				outFilters.push({ field: row.field.trim(), op: row.op });
				continue;
			}
			const coerced = coerceFilterValue(rowKind(row), String(row.value ?? ''));
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
			limit
		};
		return { request };
	}

	function buildRequest(): { request?: StructuredQueryRequest; error?: string } {
		if (!schema.data) return { error: 'Query schema is not loaded.' };
		if (mode === 'builder') return buildBuilderRequest();
		try {
			return { request: parseQueryText(code, schema.data) };
		} catch (error) {
			return { error: queryErrorMessage(error, code) };
		}
	}

	function builderSignature(): string {
		return JSON.stringify({
			dataset, selectedFields, limit,
			filters: filters.map(({ field, op, value, custom }) => ({ field, op, value, custom })),
			orderBy: orderBy.map(({ field, direction }) => ({ field, direction }))
		});
	}

	function applyCodeRequest(request: StructuredQueryRequest) {
		if (schema.data) {
			const spec = schema.data.datasets.find((d) => d.name === request.dataset)!;
			dataset = request.dataset;
			selectedFields = [...(request.fields ?? spec.default_fields)];
			columnSearch = '';
			filters = (request.filters ?? []).map((f) => {
				const kind = queryField(schema.data!, spec, f.field)?.filter_kind;
				return {
					id: nextId(), field: f.field, op: f.op,
					custom: !spec.fields.some((field) => field.name === f.field),
					value: f.value === undefined ? '' : kind === 'json' || kind === 'json_path'
						? JSON.stringify(f.value) : String(f.value)
				};
			});
			orderBy = (request.order_by ?? spec.default_order).map((o) => ({ id: nextId(), field: o.field, direction: o.direction ?? 'desc' }));
			limit = request.limit ?? 100;
		}
	}

	function changeMode(next: string) {
		if (next === mode || !schema.data) return;
		if (next === 'builder') {
			const built = buildRequest();
			if (built.request) {
				if (code !== codeAtEntry) applyCodeRequest(built.request);
				savedCodeDraft = null;
				modeNotice = null;
			} else {
				savedCodeDraft = code;
				builderAtCodeError = builderSignature();
				modeNotice = 'The code could not be converted. Your previous Builder settings and code draft have been kept.';
			}
		} else if (savedCodeDraft !== null && builderSignature() === builderAtCodeError) {
			code = savedCodeDraft;
			modeNotice = null;
		} else {
			const built = buildBuilderRequest();
			if (built.request) {
				code = formatQueryText(built.request, schema.data);
				modeNotice = null;
			} else {
				if (!code) code = formatQueryText({ dataset }, schema.data);
				modeNotice = 'The incomplete Builder settings have been kept. Continue editing your query in Code.';
			}
		}
		if (next === 'code') codeAtEntry = code;
		mode = next as 'builder' | 'code';
		runError = null;
	}

	function restoreCodeDraft() {
		if (savedCodeDraft === null) return;
		code = savedCodeDraft;
		runError = null;
		modeNotice = null;
	}

	function formatCode() {
		const built = buildRequest();
		if (built.request && schema.data) {
			code = formatQueryText(built.request, schema.data);
			runError = null;
		} else runError = built.error ?? 'Invalid query.';
	}

	const preview = $derived.by(() => {
		if (!schema.data || !dataset || mode !== 'builder') return '';
		const built = buildBuilderRequest();
		return built.request ? formatQueryText(built.request, schema.data) : '';
	});

	async function run() {
		if (running) return;
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

<PageHeader title="Query" description="Explore your traces with the visual builder or write a query." />

{#if schema.loading}
	<Card><Spinner label="Loading schema…" /></Card>
{:else if schema.error}
	<Card><ErrorState message={schema.error} onRetry={() => schema.load()} /></Card>
{:else if schema.data}
	<div class="flex min-w-0 flex-col gap-4">
		<section class="card min-w-0" aria-label="Query composer">
			<div class="composer-toolbar flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-2">
				<div class="flex flex-wrap items-center gap-3">
					<Tabs tabs={[{ id: 'builder', label: 'Builder' }, { id: 'code', label: 'Code' }]} active={mode} onChange={changeMode} />
					{#if mode === 'builder'}
						<button type="button" class="btn !border-transparent !px-2 text-xs text-fg-muted" onclick={clearBuilder} disabled={running}>
							<RotateCcwIcon size={14} /> Clear builder
						</button>
					{/if}
				</div>
				<div class="flex items-center gap-2">
					<button type="button" class="btn" onclick={exportResults} disabled={running}>
						<DownloadIcon size={15} /> Export
					</button>
					<button type="button" class="btn btn-brand" onclick={run} disabled={running}>
						{#if running}<RefreshCwIcon size={15} class="animate-spin" /> Running…{:else}<SearchIcon size={15} /> Run query{/if}
					</button>
				</div>
			</div>
			{#if modeNotice}<p class="border-b border-border px-4 py-3 text-sm text-fg-muted" role="status">{modeNotice}</p>{/if}
			{#if runError}<p class="border-b border-border px-4 py-3 text-sm text-danger" role="alert">{runError}</p>{/if}
			{#if mode === 'builder'}
				<div class="builder">
					<div class="clause-row border-b border-border">
						<label for="query-dataset" class="clause-label">From</label>
						<div class="flex min-w-0 flex-wrap items-center gap-3">
							<div class="flex items-center gap-2 text-brand">
								<DatabaseIcon size={17} />
								<select id="query-dataset" aria-label="Dataset" class="input !w-auto max-w-full pr-8 font-medium" value={dataset} onchange={(e) => onDatasetChange(e.currentTarget.value)}>
									{#each schema.data.datasets as ds (ds.name)}<option value={ds.name}>{ds.name}</option>{/each}
								</select>
							</div>
							<span class="text-xs text-fg-muted">{currentDataset?.fields.length ?? 0} available fields</span>
						</div>
					</div>

					<div role="group" aria-label="Filters · match all conditions">
						{#each filters as row, index (row.id)}
							{@const inputKind = valueInputKind(rowKind(row), row.op)}
							<div class="clause-row border-b border-border">
								<span class="clause-label">{index === 0 ? 'Where' : 'And'}</span>
								<div class="condition">
									{#if row.custom}
										<input class="input condition-field font-mono" aria-label="Plugin metadata path" placeholder="plugin_metadata.name.path" bind:value={row.field} />
									{:else}
										<select class="input condition-field pr-8 font-mono" aria-label="Filter field" value={row.field} onchange={(e) => onFilterFieldChange(row, e.currentTarget.value)}>
											{#each filterableFields as f (f.name)}<option value={f.name}>{f.name}</option>{/each}
										</select>
									{/if}
									<select class="input condition-operator pr-8" aria-label="Filter operator" value={row.op} onchange={(e) => onFilterOperatorChange(row, e.currentTarget.value as QueryOp)}>
										{#each rowOperators(row) as op (op)}<option value={op}>{operatorLabels[op]}</option>{/each}
									</select>
									{#if inputKind === 'bool'}
										<select class="input condition-value pr-8" aria-label="Filter value" bind:value={row.value}>
											<option value="true">true</option><option value="false">false</option>
										</select>
									{:else if inputKind === 'int'}
										<input class="input condition-value font-mono" aria-label="Filter value" type="number" placeholder="Enter a number" bind:value={row.value} />
									{:else if inputKind !== 'none'}
										<input class="input condition-value font-mono" aria-label="Filter value" placeholder={inputKind === 'timestamp' ? 'YYYY-MM-DDTHH:mm:ssZ' : row.custom || rowKind(row) === 'json' ? 'Text or JSON value' : 'Enter a value'} bind:value={row.value} />
									{:else}
										<span class="condition-value px-3 text-xs text-fg-muted">No value needed</span>
									{/if}
									<button type="button" class="remove-button" aria-label="Remove filter" title="Remove filter" onclick={() => removeFilter(row.id)}><XIcon size={15} /></button>
								</div>
							</div>
						{/each}
						<div class="clause-row border-b border-border">
							<span class="clause-label">{filters.length ? '' : 'Where'}</span>
							<div class="flex flex-wrap items-center gap-3">
								<select class="input !w-auto max-w-full cursor-pointer pr-8 text-brand" aria-label="Add filter" value="" disabled={filters.length >= maxFilters} onchange={(e) => { addFilter(e.currentTarget.value); e.currentTarget.value = ''; }}>
									<option value="" disabled>+ Add filter</option>
									{#each filterableFields as f (f.name)}<option value={f.name}>{f.name}</option>{/each}
									{#if supportsPluginMetadata}<option value="__plugin__">Plugin metadata path…</option>{/if}
								</select>
								<span class="text-xs text-fg-muted">{filters.length ? `${filters.length} of ${maxFilters} filters · all must match` : 'All rows match. Add a filter to narrow your search.'}</span>
							</div>
						</div>
					</div>

					<div class="clause-row border-b border-border">
						<span class="clause-label">Sort by</span>
						<div class="flex min-w-0 flex-wrap items-center gap-2" role="group" aria-label="Sort order">
							{#each orderBy as row, index (row.id)}
								<div class="sort-key">
									<span class="pl-2 text-xs text-fg-muted" title="Sort priority">{index + 1}</span>
									{#if !fieldSpec(row.field)}
										<input class="input min-w-0 flex-1 font-mono" aria-label="Sort field" bind:value={row.field} />
									{:else}
										<select class="input min-w-0 flex-1 pr-8 font-mono" aria-label="Sort field" bind:value={row.field}>
											{#each currentDataset?.fields ?? [] as f (f.name)}<option value={f.name}>{f.name}</option>{/each}
										</select>
									{/if}
									<select class="input !w-auto shrink-0 pr-8" aria-label="Sort direction" bind:value={row.direction}>
										{#each schema.data.sort_directions as dir (dir)}<option value={dir}>{dir === 'asc' ? '↑ Asc' : '↓ Desc'}</option>{/each}
									</select>
									<button type="button" class="remove-button" aria-label="Remove sort" title="Remove sort" onclick={() => removeOrder(row.id)}><XIcon size={15} /></button>
								</div>
							{/each}
							<button type="button" class="btn !border-transparent !px-2 text-xs" onclick={addOrder} disabled={orderBy.length >= maxOrder || orderBy.length >= (currentDataset?.fields.length ?? 0)}><PlusIcon size={14} /> Add sort</button>
							{#if !orderBy.length}<span class="text-xs text-fg-muted">Dataset default order</span>{/if}
						</div>
					</div>

					<details class="columns-panel">
						<summary class="clause-row cursor-pointer border-b border-border">
							<span class="clause-label">Select</span>
							<span class="flex min-w-0 flex-wrap items-center gap-2 text-sm">
								<Columns3Icon size={16} class="text-fg-muted" />
								<span class="font-medium">{selectedFields.length} columns</span>
								<span class="hidden min-w-0 flex-1 gap-1.5 overflow-hidden sm:flex" aria-hidden="true">
									{#each selectedFields.slice(0, 3) as name (name)}<span class="max-w-40 truncate rounded bg-surface-muted px-2 py-1 font-mono text-xs text-fg-muted">{name}</span>{/each}
									{#if selectedFields.length > 3}<span class="self-center text-xs text-fg-muted">+{selectedFields.length - 3}</span>{/if}
								</span>
								<span class="ml-auto flex items-center gap-1 text-xs text-brand">Edit columns <ChevronDownIcon size={14} /></span>
							</span>
						</summary>
						<div class="border-b border-border p-4 sm:pl-24">
							<div class="mb-3 flex flex-wrap items-center gap-2">
								<input class="input min-w-0 sm:!w-64" type="search" aria-label="Search columns" placeholder="Search columns…" bind:value={columnSearch} />
								<button type="button" class="btn text-xs" onclick={() => selectedFields = [...(currentDataset?.default_fields ?? [])]}>Use defaults</button>
								<button type="button" class="btn text-xs" onclick={selectAllFields} disabled={availableColumns.length > maxFields}>Select all</button>
								<span class="ml-auto text-xs text-fg-muted">{selectedFields.length} / {maxFields} selected</span>
							</div>
							<div class="grid max-h-64 gap-x-4 overflow-y-auto sm:grid-cols-2 xl:grid-cols-3">
								{#each matchingColumns as name (name)}
									<label class="flex min-w-0 cursor-pointer items-center gap-2 rounded px-2 py-2 hover:bg-surface-muted">
										<input type="checkbox" class="rounded border-border text-brand focus:ring-brand" checked={selectedFields.includes(name)} disabled={selectedFields.includes(name) ? selectedFields.length === 1 : selectedFields.length >= maxFields} onchange={() => toggleField(name)} />
										<span class="break-all font-mono text-xs">{name}</span>
									</label>
								{:else}<p class="py-2 text-sm text-fg-muted">No columns match “{columnSearch}”.</p>{/each}
							</div>
						</div>
					</details>

					<div class="clause-row rounded-b-xl bg-surface-muted/40">
						<label for="query-limit" class="clause-label">Limit</label>
						<div class="flex min-w-0 flex-wrap items-center gap-3">
							<input id="query-limit" aria-label="Row limit" class="input !w-24" type="number" min="1" max={maxLimit} bind:value={limit} />
							<span class="text-xs text-fg-muted">Up to {maxLimit} rows</span>
							{#if preview}
								<details class="query-preview ml-auto min-w-0 text-xs">
									<summary class="cursor-pointer text-fg-muted">Query preview</summary>
									<pre class="mt-3 overflow-x-auto rounded-lg border border-border bg-surface p-3 leading-6 text-fg-muted">{preview}</pre>
								</details>
							{/if}
						</div>
					</div>
				</div>
			{:else}
				<div class="p-4">
					<div class="mb-3 flex items-center justify-between gap-2">
						<p class="text-sm text-fg-muted">Filter, sort and select your results</p>
						<button type="button" class="btn !px-2 !py-1 text-xs" onclick={formatCode}>Format query</button>
					</div>
					{#if savedCodeDraft !== null && code !== savedCodeDraft}
						<div class="mb-3 flex flex-wrap items-center gap-2 text-sm">
							<span class="text-fg-muted">Your previous code draft is saved.</span>
							<button type="button" class="cursor-pointer underline underline-offset-2" onclick={restoreCodeDraft}>Restore code draft</button>
						</div>
					{/if}
					<QueryCodeEditor bind:value={code} schema={schema.data} onRun={run} />
					<p id="query-editor-help" class="text-fg-muted mt-2 text-xs">Enter to run · Shift+Enter for a new line · Ctrl+Space for suggestions · Tab or Enter to accept · Esc to dismiss</p>
					<details class="mt-3 text-sm">
						<summary class="text-fg-muted cursor-pointer">Syntax & examples</summary>
						<div class="text-fg-muted mt-2 space-y-2 text-xs">
							<p>Query {schema.data.datasets.map((d) => d.name).join(', ')}. Combine filters with AND. Use =, !=, &gt;, &gt;=, &lt;, &lt;=, CONTAINS, IS NULL or IS NOT NULL when the field supports them.</p>
							<pre class="overflow-x-auto rounded-lg bg-[var(--color-surface-muted)] p-3">{`SELECT id, model, status, duration_ms\nFROM requests\nWHERE status >= 400\nORDER BY started_at DESC\nLIMIT 100;`}</pre>
							<p>Quote text with single quotes. Timestamps include a timezone, for example '2026-09-22T00:00:00+08:00'. Use JSON '{'{"team":"research"}'}' for a JSON value.</p>
							<p>One SELECT per query, up to {maxLimit} rows. Joins, OR, aggregations and subqueries are not supported. Switching to Builder converts valid conditions; invalid code is kept as a draft while your previous Builder settings are shown.</p>
						</div>
					</details>
				</div>
			{/if}
		</section>

		<div class="min-w-0">
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
					<div class="p-6"><EmptyState icon={ChartNoAxesColumnIcon} title="No results yet" message="Choose your filters above, then run the query to explore matching rows." /></div>
				{:else if result.rows.length === 0}
					<div class="p-6"><EmptyState icon={InboxIcon} title="No rows" message="The query returned no rows." /></div>
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

<style>
	.builder { --control-height: 36px; }
	.composer-toolbar, .clause-row { min-height: 56px; }
	.clause-row { display: grid; grid-template-columns: 4rem minmax(0, 1fr); gap: 1rem; align-items: center; padding: 8px 16px; }
	.builder .input, .builder .btn { height: var(--control-height); }
	.clause-label { color: var(--color-fg-muted); font-size: 0.65rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
	.condition { display: grid; grid-template-columns: minmax(10rem, 1fr) 10rem minmax(10rem, 1.4fr) 2.5rem; align-items: center; border: 1px solid var(--color-border); border-radius: 0.5rem; }
	.condition:focus-within { border-color: var(--color-brand); }
	.condition .input, .sort-key .input { height: calc(var(--control-height) - 2px); min-width: 0; border: 0; border-radius: 0; background-color: transparent; font-size: 0.8rem; }
	.condition .input:focus, .sort-key .input:focus { outline-offset: -2px; }
	.condition .condition-field, .condition .condition-operator { border-right: 1px solid var(--color-border); }
	.condition-operator { color: var(--color-fg-muted); }
	.remove-button { display: inline-flex; align-items: center; justify-content: center; width: 2.5rem; height: calc(var(--control-height) - 2px); flex-shrink: 0; border-radius: 0.4rem; color: var(--color-fg-muted); cursor: pointer; }
	.remove-button:hover { color: var(--color-danger); background: var(--color-surface-muted); }
	.sort-key { display: flex; align-items: center; width: 23rem; max-width: 100%; border: 1px solid var(--color-border); border-radius: 0.5rem; }
	.columns-panel > summary { list-style: none; }
	.columns-panel > summary::-webkit-details-marker { display: none; }
	.columns-panel > summary:hover { background: var(--color-surface-muted); }
	.query-preview[open] { width: 100%; }
	@media (max-width: 1023px) {
		.condition { grid-template-columns: minmax(0, 1fr) 9.5rem 2.5rem; }
		.condition .condition-field { grid-column: 1 / 3; border-right: 0; border-bottom: 1px solid var(--color-border); }
		.condition .condition-operator { grid-column: 1; grid-row: 2; }
		.condition-value { grid-column: 2 / 4; grid-row: 2; }
		.condition .remove-button { grid-column: 3; grid-row: 1; }
	}
	@media (max-width: 767px) {
		.builder { --control-height: 46px; }
		.clause-row { min-height: 64px; }
	}
	@media (max-width: 639px) {
		.clause-row { grid-template-columns: minmax(0, 1fr); gap: 0.5rem; }
		.clause-label:empty { display: none; }
		.condition .input, .sort-key .input { font-size: 1rem; }
		.remove-button { width: 2.75rem; height: 2.75rem; }
		.condition { grid-template-columns: minmax(10rem, 1fr) minmax(0, 7rem) 2.75rem; }
		.sort-key { width: 100%; }
		.columns-panel > summary { min-height: 44px; }
	}
</style>
