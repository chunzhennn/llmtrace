import { describe, it, expect } from 'vitest';
import {
	formatNumber,
	formatBytes,
	formatDuration,
	formatMs,
	formatPercent,
	statusTone,
	truncateMiddle,
	safeJsonParse,
	formatSecondsDuration
} from './format';

describe('formatNumber', () => {
	it('returns a dash for nullish/NaN', () => {
		expect(formatNumber(null)).toBe('-');
		expect(formatNumber(undefined)).toBe('-');
		expect(formatNumber(Number.NaN)).toBe('-');
	});
	it('formats integers', () => {
		expect(formatNumber(0)).toBe('0');
		expect(formatNumber(1234)).toBe(new Intl.NumberFormat().format(1234));
	});
});

describe('formatBytes', () => {
	it('handles zero and nullish', () => {
		expect(formatBytes(0)).toBe('0 B');
		expect(formatBytes(null)).toBe('-');
	});
	it('scales to KB/MB', () => {
		expect(formatBytes(1024)).toBe('1.0 KB');
		expect(formatBytes(1024 * 1024)).toBe('1.0 MB');
		expect(formatBytes(1536)).toBe('1.5 KB');
	});
	it('drops decimals for large values', () => {
		expect(formatBytes(1024 * 512)).toBe('512 KB');
	});
});

describe('formatDuration', () => {
	it('handles sub-ms and ms', () => {
		expect(formatDuration(0.5)).toBe('<1 ms');
		expect(formatDuration(250)).toBe('250 ms');
	});
	it('formats seconds and minutes', () => {
		expect(formatDuration(1500)).toBe('1.50 s');
		expect(formatDuration(65000)).toBe('1m 5s');
	});
	it('returns dash for nullish', () => {
		expect(formatDuration(null)).toBe('-');
	});
});

describe('formatMs', () => {
	it('rounds and appends unit', () => {
		expect(formatMs(12.4)).toBe('12 ms');
		expect(formatMs(null)).toBe('-');
	});
});

describe('formatPercent', () => {
	it('multiplies ratio by 100', () => {
		expect(formatPercent(0.1234)).toBe('12.3%');
		expect(formatPercent(0)).toBe('0.0%');
		expect(formatPercent(null)).toBe('-');
	});
});

describe('statusTone', () => {
	it('maps status ranges to tones', () => {
		expect(statusTone(200)).toBe('success');
		expect(statusTone(301)).toBe('info');
		expect(statusTone(404)).toBe('warning');
		expect(statusTone(500)).toBe('danger');
		expect(statusTone(null)).toBe('neutral');
	});
});

describe('truncateMiddle', () => {
	it('keeps short strings intact', () => {
		expect(truncateMiddle('short', 40)).toBe('short');
	});
	it('truncates long strings in the middle', () => {
		const out = truncateMiddle('abcdefghijklmnopqrstuvwxyz', 11);
		expect(out).toContain('…');
		expect(out.length).toBeLessThanOrEqual(11);
	});
});

describe('safeJsonParse', () => {
	it('parses valid json', () => {
		expect(safeJsonParse('{"a":1}')).toEqual({ ok: true, value: { a: 1 } });
	});
	it('reports failure for invalid json', () => {
		expect(safeJsonParse('not json')).toEqual({ ok: false, value: null });
	});
});

describe('formatSecondsDuration', () => {
	it('formats compact durations', () => {
		expect(formatSecondsDuration(30)).toBe('30s');
		expect(formatSecondsDuration(90)).toBe('1m');
		expect(formatSecondsDuration(3700)).toBe('1h 1m');
		expect(formatSecondsDuration(-1)).toBe('expired');
	});
});
