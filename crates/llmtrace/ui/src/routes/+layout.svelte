<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import Toasts from '$lib/components/Toasts.svelte';
	import { theme } from '$lib/state/theme.svelte';

	let { children } = $props();

	onMount(() => {
		theme.init();
	});
</script>

{@render children()}

<Toasts />
