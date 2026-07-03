<script lang="ts">
	interface Props {
		class?: string;
		width?: string;
		height?: string;
	}

	let { class: className = '', width, height = '1rem' }: Props = $props();
</script>

<div
	class="animate-pulse rounded {className}"
	style="background-color: var(--color-surface-muted); {width ? `width: ${width};` : ''} height: {height};"
></div>
