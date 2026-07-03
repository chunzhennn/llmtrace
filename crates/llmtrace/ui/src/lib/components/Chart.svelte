<script lang="ts">
	import { untrack } from 'svelte';
	import Chart from 'chart.js/auto';
	import type { ChartData, ChartOptions, ChartType } from 'chart.js';

	interface Props {
		type: ChartType;
		data: ChartData;
		options?: ChartOptions;
		height?: string;
	}

	let { type, data, options = {}, height = '16rem' }: Props = $props();

	let chart = $state<Chart | undefined>();

	// Lifecycle attachment: create the chart on mount and recreate it if the
	// chart `type` changes. Data/options are read untracked here so ongoing
	// updates are handled by the effect below instead of full recreations.
	function mountChart(node: HTMLCanvasElement) {
		const instance = new Chart(node, {
			type,
			data: untrack(() => data),
			options: untrack(() => options)
		});
		chart = instance;
		return () => {
			instance.destroy();
			chart = undefined;
		};
	}

	$effect(() => {
		const nextData = data;
		const nextOptions = options;
		const instance = chart;
		if (!instance) return;
		instance.data = nextData;
		instance.options = nextOptions;
		instance.update();
	});
</script>

<div style="position: relative; height: {height};">
	<canvas {@attach mountChart}></canvas>
</div>
