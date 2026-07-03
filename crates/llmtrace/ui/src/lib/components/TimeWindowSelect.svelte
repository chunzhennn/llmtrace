<script lang="ts" module>
	export interface TimeWindowOption {
		label: string;
		hours: number;
	}

	export const DEFAULT_WINDOWS: TimeWindowOption[] = [
		{ label: '1h', hours: 1 },
		{ label: '6h', hours: 6 },
		{ label: '24h', hours: 24 },
		{ label: '7d', hours: 168 },
		{ label: '30d', hours: 720 }
	];
</script>

<script lang="ts">
	interface Props {
		value: number;
		options?: TimeWindowOption[];
		onChange: (hours: number) => void;
	}

	let { value, options = DEFAULT_WINDOWS, onChange }: Props = $props();
</script>

<div
	class="inline-flex overflow-hidden rounded-lg border"
	style="border-color: var(--color-border);"
	role="group"
	aria-label="Time window"
>
	{#each options as option, i (option.hours)}
		<button
			type="button"
			class="px-3 py-1.5 text-xs font-medium transition-colors"
			style={value === option.hours
				? 'background-color: var(--color-brand); color: var(--color-brand-fg);'
				: 'color: var(--color-fg);'}
			class:border-l={i > 0}
			onclick={() => onChange(option.hours)}
			aria-pressed={value === option.hours}
		>
			{option.label}
		</button>
	{/each}
</div>
