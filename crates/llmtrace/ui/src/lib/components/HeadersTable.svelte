<script lang="ts">
	import Badge from './Badge.svelte';
	import EmptyState from './EmptyState.svelte';
	import type { HeaderValue, RedactedHeader } from '$lib/api/types';

	interface Props {
		headers: Record<string, HeaderValue>;
	}

	let { headers }: Props = $props();

	const entries = $derived(Object.entries(headers ?? {}).sort((a, b) => a[0].localeCompare(b[0])));

	function isRedacted(value: HeaderValue): value is RedactedHeader {
		return typeof value === 'object' && value !== null && 'redacted' in value;
	}

	function maskedText(value: RedactedHeader): string {
		if (value.url) return value.url;
		const scheme = value.scheme ? `${value.scheme} ` : '';
		const prefix = value.prefix ?? '';
		const suffix = value.suffix ?? '';
		return `${scheme}${prefix}…${suffix}`;
	}
</script>

{#if entries.length === 0}
	<EmptyState icon="inbox" title="No headers captured" />
{:else}
	<div class="overflow-x-auto">
		<table class="w-full border-collapse text-sm">
			<tbody>
				{#each entries as [name, value] (name)}
					<tr style="border-bottom: 1px solid var(--color-border);">
						<td
							class="text-fg-muted whitespace-nowrap px-3 py-2 align-top font-mono text-xs font-medium"
							style="width: 12rem;"
						>
							{name}
						</td>
						<td class="px-3 py-2 align-top font-mono text-xs break-all">
							{#if isRedacted(value)}
								<div class="flex flex-wrap items-center gap-2">
									<span>{maskedText(value)}</span>
									<Badge tone="warning">redacted</Badge>
									{#if value.length !== undefined}
										<span class="text-fg-muted">len {value.length}</span>
									{/if}
								</div>
								{#if value.sha256}
									<div class="text-fg-muted mt-1 text-[0.7rem]">sha256: {value.sha256}</div>
								{/if}
							{:else}
								{value}
							{/if}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}
