<script lang="ts">
	import Badge, { type BadgeTone } from './Badge.svelte';
	import { statusTone } from '$lib/utils/format';

	interface Props {
		status: number | null | undefined;
		error?: string | null;
	}

	let { status, error }: Props = $props();

	const tone = $derived<BadgeTone>(error ? 'danger' : statusTone(status));
	const label = $derived(status === null || status === undefined ? (error ? 'ERR' : '—') : String(status));
</script>

<Badge {tone}>{label}</Badge>
