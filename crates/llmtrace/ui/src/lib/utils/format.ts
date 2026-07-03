// Formatting helpers shared across the dashboard.

export function formatNumber(value: number | null | undefined): string {
	if (value === null || value === undefined || Number.isNaN(value)) return '-';
	return new Intl.NumberFormat().format(value);
}

export function formatBytes(value: number | null | undefined): string {
	if (value === null || value === undefined || Number.isNaN(value)) return '-';
	if (value === 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
	const exponent = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
	const scaled = value / Math.pow(1024, exponent);
	const digits = scaled >= 100 || exponent === 0 ? 0 : 1;
	return `${scaled.toFixed(digits)} ${units[exponent]}`;
}

export function formatDuration(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || Number.isNaN(ms)) return '-';
	if (ms < 1) return '<1 ms';
	if (ms < 1000) return `${Math.round(ms)} ms`;
	const seconds = ms / 1000;
	if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 2 : 1)} s`;
	const minutes = Math.floor(seconds / 60);
	const rem = Math.round(seconds % 60);
	return `${minutes}m ${rem}s`;
}

export function formatMs(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || Number.isNaN(ms)) return '-';
	return `${Math.round(ms)} ms`;
}

export function formatPercent(value: number | null | undefined, digits = 1): string {
	if (value === null || value === undefined || Number.isNaN(value)) return '-';
	return `${(value * 100).toFixed(digits)}%`;
}

export function formatDateTime(value: string | null | undefined): string {
	if (!value) return '-';
	const date = new Date(value);
	if (Number.isNaN(date.getTime())) return value;
	return date.toLocaleString(undefined, {
		year: 'numeric',
		month: 'short',
		day: '2-digit',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
}

export function formatDate(value: string | null | undefined): string {
	if (!value) return '-';
	const date = new Date(value);
	if (Number.isNaN(date.getTime())) return value;
	return date.toLocaleDateString(undefined, {
		year: 'numeric',
		month: 'short',
		day: '2-digit'
	});
}

export function formatRelativeTime(value: string | null | undefined): string {
	if (!value) return '-';
	const date = new Date(value);
	if (Number.isNaN(date.getTime())) return value;
	const diffMs = date.getTime() - Date.now();
	const abs = Math.abs(diffMs);
	if (abs < 1000) return 'just now';

	const divisions: [number, Intl.RelativeTimeFormatUnit][] = [
		[1000, 'second'],
		[60 * 1000, 'minute'],
		[60 * 60 * 1000, 'hour'],
		[24 * 60 * 60 * 1000, 'day'],
		[7 * 24 * 60 * 60 * 1000, 'week'],
		[30 * 24 * 60 * 60 * 1000, 'month'],
		[365 * 24 * 60 * 60 * 1000, 'year']
	];
	let unitMs = 1000;
	let unit: Intl.RelativeTimeFormatUnit = 'second';
	for (const [threshold, candidate] of divisions) {
		if (abs >= threshold) {
			unitMs = threshold;
			unit = candidate;
		}
	}
	const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
	return rtf.format(Math.round(diffMs / unitMs), unit);
}

export function formatSecondsDuration(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || Number.isNaN(seconds)) return '-';
	if (seconds < 0) return 'expired';
	if (seconds < 60) return `${Math.round(seconds)}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ${minutes % 60}m`;
	const days = Math.floor(hours / 24);
	return `${days}d ${hours % 24}h`;
}

export type StatusTone = 'success' | 'info' | 'warning' | 'danger' | 'neutral';

export function statusTone(status: number | null | undefined): StatusTone {
	if (status === null || status === undefined) return 'neutral';
	if (status >= 200 && status < 300) return 'success';
	if (status >= 300 && status < 400) return 'info';
	if (status >= 400 && status < 500) return 'warning';
	if (status >= 500) return 'danger';
	return 'neutral';
}

export function truncateMiddle(value: string, max = 40): string {
	if (value.length <= max) return value;
	const half = Math.floor((max - 1) / 2);
	return `${value.slice(0, half)}…${value.slice(value.length - half)}`;
}

export function safeJsonParse(text: string): { ok: boolean; value: unknown } {
	try {
		return { ok: true, value: JSON.parse(text) };
	} catch {
		return { ok: false, value: null };
	}
}
