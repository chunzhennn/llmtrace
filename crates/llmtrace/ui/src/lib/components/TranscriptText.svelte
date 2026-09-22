<script lang="ts">
	let { content }: { content: string } = $props();
</script>

<pre class="transcript-text min-w-0 whitespace-pre-wrap font-sans [overflow-wrap:anywhere]">{content}</pre>

<style>
	/* One text flow preserves wrapping and copied line breaks; defer off-screen messages. */
	.transcript-text { content-visibility: auto; contain-intrinsic-size: auto 16rem; }
</style>
