<script lang="ts">
	import Icon from './Icon.svelte';

	interface Props {
		text: string | (() => string);
		label?: string;
		class?: string;
	}

	let { text, label = 'Copy', class: className = '' }: Props = $props();

	let copied = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	async function copy() {
		try {
			await navigator.clipboard.writeText(typeof text === 'function' ? text() : text);
			copied = true;
			clearTimeout(timer);
			timer = setTimeout(() => (copied = false), 1500);
		} catch {
			copied = false;
		}
	}
</script>

<button type="button" class="btn !px-2 !py-1 text-xs {className}" onclick={copy} title={label}>
	<Icon name={copied ? 'check' : 'copy'} size={14} />
	{copied ? 'Copied' : label}
</button>
