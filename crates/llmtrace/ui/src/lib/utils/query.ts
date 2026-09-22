// Helpers for the structured query builder: mapping a field's filter_kind to the
// right value-input widget and coercing the raw string input into the JSON type
// the backend expects (see storage.rs value_as_* helpers).

import type { QueryOp } from '$lib/api/types';
import { safeJsonParse } from './format';

export type ValueInputKind = 'none' | 'text' | 'int' | 'bool' | 'timestamp';

export function opNeedsValue(op: QueryOp): boolean {
	return op !== 'is_null' && op !== 'is_not_null';
}

/** Which HTML input to render for a given filter_kind + operator. */
export function valueInputKind(filterKind: string | null, op: QueryOp): ValueInputKind {
	if (!opNeedsValue(op)) return 'none';
	switch (filterKind) {
		case 'int':
			return 'int';
		case 'bool':
			return 'bool';
		case 'timestamp':
			return 'timestamp';
		default:
			return 'text';
	}
}

export type CoerceResult = { ok: true; value: unknown } | { ok: false; error: string };

const RFC3339 = /^(\d{4})-(\d\d)-(\d\d)T\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/i;

export function isRfc3339Timestamp(value: string): boolean {
	const match = RFC3339.exec(value);
	if (!match || Number.isNaN(Date.parse(value)) || Number(value.slice(11, 13)) > 23) return false;
	const [, year, month, day] = match.map(Number);
	const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
	const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
	return month >= 1 && month <= 12 && day >= 1 && day <= days[month - 1];
}

/** Coerce a raw string input into the JSON value the API expects for filterKind. */
export function coerceFilterValue(filterKind: string | null, raw: string): CoerceResult {
	switch (filterKind) {
		case 'int': {
			if (raw.trim() === '') return { ok: false, error: 'Enter an integer' };
			const n = Number(raw);
			if (!Number.isFinite(n) || !Number.isInteger(n)) return { ok: false, error: 'Enter an integer' };
			return { ok: true, value: n };
		}
		case 'bool':
			return { ok: true, value: raw === 'true' };
		case 'timestamp': {
			if (raw.trim() === '') return { ok: false, error: 'Enter a date/time' };
			const date = new Date(raw);
			if (Number.isNaN(date.getTime())) return { ok: false, error: 'Enter a valid date/time' };
			// Preserve explicit offsets and sub-millisecond precision when switching editor modes.
			if (RFC3339.test(raw)) {
				if (!isRfc3339Timestamp(raw)) return { ok: false, error: 'Enter a valid date/time' };
				return { ok: true, value: raw };
			}
			return { ok: true, value: date.toISOString() };
		}
		case 'json':
		case 'json_path': {
			// Accept a JSON literal (number/bool/object/array/string); fall back to the
			// raw text so a bare word is sent as a JSON string.
			const parsed = safeJsonParse(raw);
			if (parsed.ok && parsed.value === null) {
				return { ok: false, error: 'Standalone JSON null is not supported. Use IS NULL or IS NOT NULL.' };
			}
			return { ok: true, value: parsed.ok ? parsed.value : raw };
		}
		default:
			// text, uuid, text_array, or unfilterable: send as a string.
			return { ok: true, value: raw };
	}
}

/** Render a result cell value (unknown JSON) as a display string. */
export function displayCell(value: unknown): string {
	if (value === null || value === undefined) return '';
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean') return String(value);
	try {
		return JSON.stringify(value);
	} catch {
		return String(value);
	}
}
