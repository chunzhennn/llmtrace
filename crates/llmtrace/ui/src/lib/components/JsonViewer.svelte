<script lang="ts">
	import CopyButton from './CopyButton.svelte';
	import { jsonPreview, jsonText } from '$lib/utils/json-preview';

	interface Props {
		value: unknown;
		maxHeight?: string;
	}

	let { value, maxHeight = '28rem' }: Props = $props();

	const preview = $derived(jsonPreview(value));
</script>

{#if preview.truncated}<p class="text-fg-muted mb-2 text-xs">Large JSON preview shortened for responsiveness. Copy retrieves the complete JSON.</p>{/if}
<div class="relative">
	<div class="absolute right-2 top-2 z-10">
		<CopyButton text={() => jsonText(value)} />
	</div>
	<pre
		class="overflow-auto rounded-lg p-3 text-xs leading-relaxed"
		style="background-color: var(--color-surface-muted); max-height: {maxHeight};"><code>{preview.text}</code></pre>
</div>
