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

	function navigate(event: KeyboardEvent, index: number) {
		let next: number;
		if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
		else if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = tabs.length - 1;
		else return;
		event.preventDefault();
		const button = event.currentTarget as HTMLButtonElement;
		const target = button.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next];
		target?.focus({ preventScroll: true });
		target?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		onChange(tabs[next].id);
	}
</script>

<div class="flex gap-1 overflow-x-auto overflow-y-hidden" style="border-bottom: 1px solid var(--color-border);" role="tablist" aria-label="Views">
	{#each tabs as tab, index (tab.id)}
		<button
			type="button"
			role="tab"
			aria-selected={active === tab.id}
			tabindex={active === tab.id ? 0 : -1}
			onkeydown={(event) => navigate(event, index)}
			class="whitespace-nowrap px-3 py-2 text-sm font-medium transition-colors"
			style={active === tab.id
				? 'color: var(--color-brand); border-bottom: 2px solid var(--color-brand);'
				: 'color: var(--color-fg-muted); border-bottom: 2px solid transparent;'}
			onclick={() => onChange(tab.id)}
		>
			{tab.label}
		</button>
	{/each}
</div>
