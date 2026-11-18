<script lang="ts">
	import { requestPromptIndex } from '$lib/api/endpoints/query';
	import { createResource } from '$lib/utils/resource.svelte';
	let { id }: { id: string } = $props();
	const status = createResource(signal => requestPromptIndex(id, signal), () => id);
</script>

<aside class="card mb-4 p-3 text-sm" aria-label="Request tag index">
	{#if status.loading}<p>Loading request tag index status…</p>
	{:else if status.error}<p>Request tag coverage is unknown: {status.error}</p>
	{:else if status.data}
		<p>Input prompt tag index v{status.data.index_version}: {status.data.state}</p>
		{#if status.data.index_version !== 1 || !['disabled', 'not_indexed', 'pending', 'processing', 'ready', 'partial', 'unsupported', 'failed'].includes(status.data.state)}<p>Unknown index format. Refresh the app to check compatibility.</p>{/if}
		<p class="text-fg-muted">Request capture: {status.data.capture_complete ? 'complete' : 'truncated'}. {status.data.external_history ? 'Provider-managed history is not captured.' : 'Coverage applies only to text carried by this request.'}</p>
		{#if status.data.diagnostics.length}<p class="text-fg-muted break-all">{status.data.diagnostics.join(', ')}</p>{/if}
	{/if}
</aside>
