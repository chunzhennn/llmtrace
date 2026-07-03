<script lang="ts" module>
	export interface Column {
		label: string;
		align?: 'left' | 'right' | 'center';
		class?: string;
	}
</script>

<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import EmptyState from './EmptyState.svelte';
	import ErrorState from './ErrorState.svelte';

	interface Props {
		columns: Column[];
		rows: T[];
		row: Snippet<[T, number]>;
		loading?: boolean;
		error?: string | null;
		emptyTitle?: string;
		emptyMessage?: string;
		onRetry?: () => void;
		skeletonRows?: number;
	}

	let {
		columns,
		rows,
		row,
		loading = false,
		error = null,
		emptyTitle = 'No results',
		emptyMessage,
		onRetry,
		skeletonRows = 6
	}: Props = $props();

	function alignClass(align: Column['align']): string {
		if (align === 'right') return 'text-right';
		if (align === 'center') return 'text-center';
		return 'text-left';
	}
</script>

<div class="card overflow-hidden">
	<div class="overflow-x-auto">
		<table class="w-full border-collapse text-sm">
			<thead>
				<tr style="border-bottom: 1px solid var(--color-border);">
					{#each columns as col (col.label)}
						<th
							class="text-fg-muted whitespace-nowrap px-3 py-2.5 text-xs font-semibold uppercase tracking-wide {alignClass(
								col.align
							)} {col.class ?? ''}"
						>
							{col.label}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#if loading}
					{#each Array(skeletonRows) as _, rowIndex (rowIndex)}
						<tr style="border-bottom: 1px solid var(--color-border);">
							{#each columns as col (col.label)}
								<td class="px-3 py-2.5">
									<div
										class="h-4 animate-pulse rounded"
										style="background-color: var(--color-surface-muted); width: {40 +
											((rowIndex + columns.indexOf(col)) % 4) * 15}%;"
									></div>
								</td>
							{/each}
						</tr>
					{/each}
				{:else if error}
					<tr>
						<td colspan={columns.length} class="p-0">
							<ErrorState message={error} {onRetry} />
						</td>
					</tr>
				{:else if rows.length === 0}
					<tr>
						<td colspan={columns.length} class="p-0">
							<EmptyState title={emptyTitle} message={emptyMessage} />
						</td>
					</tr>
				{:else}
					{#each rows as item, index (index)}
						<tr
							class="transition-colors hover:brightness-95 dark:hover:brightness-110"
							style="border-bottom: 1px solid var(--color-border);"
						>
							{@render row(item, index)}
						</tr>
					{/each}
				{/if}
			</tbody>
		</table>
	</div>
</div>
