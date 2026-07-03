<script lang="ts" module>
	export interface Tab {
		id: string;
		label: string;
	}
</script>

<script lang="ts">
	interface Props {
		tabs: Tab[];
		active: string;
		onChange: (id: string) => void;
	}

	let { tabs, active, onChange }: Props = $props();
</script>

<div class="flex gap-1 overflow-x-auto" style="border-bottom: 1px solid var(--color-border);" role="tablist">
	{#each tabs as tab (tab.id)}
		<button
			type="button"
			role="tab"
			aria-selected={active === tab.id}
			class="-mb-px whitespace-nowrap px-3 py-2 text-sm font-medium transition-colors"
			style={active === tab.id
				? 'color: var(--color-brand); border-bottom: 2px solid var(--color-brand);'
				: 'color: var(--color-fg-muted); border-bottom: 2px solid transparent;'}
			onclick={() => onChange(tab.id)}
		>
			{tab.label}
		</button>
	{/each}
</div>
