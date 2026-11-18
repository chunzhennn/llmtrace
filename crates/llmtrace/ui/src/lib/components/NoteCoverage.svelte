<script lang="ts">
	import type { NoteSearchCoverage } from '$lib/api/types';
	let { search }: { search: NoteSearchCoverage } = $props();
</script>

<aside class="border-b border-border bg-surface-muted p-3 text-sm" aria-label="tag index coverage" role="status">
	<p>Captured request prompt tags · index v{search.index_version} · coverage: {search.coverage}</p>
	{#if search.index_version !== 1}<p>Unknown index version. Refresh the app to check compatibility.</p>{/if}
	{#each search.warnings as warning}<p class="text-fg-muted mt-1">{warning}</p>{/each}
	{#if search.counts}
		<p class="text-fg-muted mt-1 text-xs">Retained-request snapshot: pending {search.counts.pending}, processing {search.counts.processing}, partial {search.counts.partial}, unsupported {search.counts.unsupported}, failed {search.counts.failed}, missing source {search.counts.missing_source}, missing state {search.counts.missing_state}.</p>
	{/if}
	{#if search.coverage_checked_at}<p class="text-fg-muted mt-1 text-xs">Snapshot checked {search.coverage_checked_at}; counts are not exact for this query.</p>{/if}
</aside>
