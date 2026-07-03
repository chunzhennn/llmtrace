import type { ChartOptions } from 'chart.js';

function cssVar(name: string, fallback: string): string {
	if (typeof window === 'undefined') return fallback;
	const value = getComputedStyle(document.documentElement).getPropertyValue(name);
	return value.trim() || fallback;
}

export function chartColors() {
	return {
		brand: cssVar('--color-brand', '#6366f1'),
		danger: cssVar('--color-danger', '#f87171'),
		success: cssVar('--color-success', '#22c55e'),
		info: cssVar('--color-info', '#38bdf8'),
		warning: cssVar('--color-warning', '#f59e0b'),
		grid: cssVar('--color-border', '#26324a'),
		fg: cssVar('--color-fg-muted', '#93a1b5')
	};
}

export function withAlpha(hex: string, alpha: number): string {
	const normalized = hex.replace('#', '');
	if (normalized.length !== 6) return hex;
	const r = parseInt(normalized.slice(0, 2), 16);
	const g = parseInt(normalized.slice(2, 4), 16);
	const b = parseInt(normalized.slice(4, 6), 16);
	return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export function baseTimeChartOptions(): ChartOptions {
	const colors = chartColors();
	return {
		responsive: true,
		maintainAspectRatio: false,
		interaction: { mode: 'index', intersect: false },
		plugins: {
			legend: {
				display: true,
				position: 'top',
				labels: { color: colors.fg, boxWidth: 12, boxHeight: 12, usePointStyle: true }
			},
			tooltip: {
				backgroundColor: cssVar('--color-surface', '#111826'),
				titleColor: cssVar('--color-fg', '#e5e9f0'),
				bodyColor: cssVar('--color-fg', '#e5e9f0'),
				borderColor: colors.grid,
				borderWidth: 1
			}
		},
		scales: {
			x: {
				grid: { color: colors.grid, display: false },
				ticks: { color: colors.fg, maxRotation: 0, autoSkip: true, maxTicksLimit: 8 }
			},
			y: {
				beginAtZero: true,
				grid: { color: withAlpha('#808080', 0.15) },
				ticks: { color: colors.fg, precision: 0 }
			}
		}
	};
}
