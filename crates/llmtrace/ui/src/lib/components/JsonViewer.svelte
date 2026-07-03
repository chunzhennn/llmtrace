<script lang="ts">
	import CopyButton from './CopyButton.svelte';

	interface Props {
		value: unknown;
		maxHeight?: string;
	}

	let { value, maxHeight = '28rem' }: Props = $props();

	const pretty = $derived.by(() => {
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return String(value);
		}
	});
</script>

<div class="relative">
	<div class="absolute right-2 top-2 z-10">
		<CopyButton text={pretty} />
	</div>
	<pre
		class="overflow-auto rounded-lg p-3 text-xs leading-relaxed"
		style="background-color: var(--color-surface-muted); max-height: {maxHeight};"><code>{pretty}</code></pre>
</div>
