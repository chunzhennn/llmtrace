<script lang="ts">
	import { Toaster } from 'svelte-sonner';
	import { theme } from '$lib/state/theme.svelte';
</script>

<Toaster
	position="bottom-right"
	theme={theme.dark ? 'dark' : 'light'}
	duration={5000}
	offset="16px"
	closeButton
	richColors
	style="--normal-bg: var(--color-surface); --normal-text: var(--color-fg); --normal-border: var(--color-border);"
	toastOptions={{ classes: { title: 'break-words' } }}
/>
