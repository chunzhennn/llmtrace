import { describe, it, expect } from 'vitest';
import { coerceFilterValue, valueInputKind, opNeedsValue, displayCell } from './query';

describe('opNeedsValue', () => {
	it('is false for null checks', () => {
		expect(opNeedsValue('is_null')).toBe(false);
		expect(opNeedsValue('is_not_null')).toBe(false);
	});
	it('is true for comparisons', () => {
		expect(opNeedsValue('eq')).toBe(true);
		expect(opNeedsValue('contains')).toBe(true);
		expect(opNeedsValue('gt')).toBe(true);
	});
});

describe('valueInputKind', () => {
	it('returns none when the operator needs no value', () => {
		expect(valueInputKind('int', 'is_null')).toBe('none');
		expect(valueInputKind('text', 'is_not_null')).toBe('none');
	});
	it('maps filter kinds to input widgets', () => {
		expect(valueInputKind('int', 'eq')).toBe('int');
		expect(valueInputKind('bool', 'eq')).toBe('bool');
		expect(valueInputKind('timestamp', 'gt')).toBe('timestamp');
		expect(valueInputKind('text', 'contains')).toBe('text');
		expect(valueInputKind('uuid', 'eq')).toBe('text');
		expect(valueInputKind('json_path', 'contains')).toBe('text');
		expect(valueInputKind(null, 'eq')).toBe('text');
	});
});

describe('coerceFilterValue', () => {
	it('coerces integers and rejects invalid ones', () => {
		expect(coerceFilterValue('int', '42')).toEqual({ ok: true, value: 42 });
		expect(coerceFilterValue('int', '')).toEqual({ ok: false, error: 'Enter an integer' });
		expect(coerceFilterValue('int', '4.5')).toEqual({ ok: false, error: 'Enter an integer' });
		expect(coerceFilterValue('int', 'abc')).toEqual({ ok: false, error: 'Enter an integer' });
	});

	it('coerces booleans', () => {
		expect(coerceFilterValue('bool', 'true')).toEqual({ ok: true, value: true });
		expect(coerceFilterValue('bool', 'false')).toEqual({ ok: true, value: false });
	});

	it('coerces timestamps to RFC3339', () => {
		const result = coerceFilterValue('timestamp', '2026-07-01T12:00');
		expect(result.ok).toBe(true);
		if (result.ok) {
			expect(typeof result.value).toBe('string');
			expect(new Date(result.value as string).toISOString()).toBe(result.value);
		}
		expect(coerceFilterValue('timestamp', 'nope').ok).toBe(false);
		expect(coerceFilterValue('timestamp', '').ok).toBe(false);
	});

	it('parses json values but falls back to a raw string', () => {
		expect(coerceFilterValue('json', '{"a":1}')).toEqual({ ok: true, value: { a: 1 } });
		expect(coerceFilterValue('json_path', '123')).toEqual({ ok: true, value: 123 });
		expect(coerceFilterValue('json_path', 'bareword')).toEqual({ ok: true, value: 'bareword' });
	});
	it('rejects standalone null while retaining JSON strings and nested nulls', () => {
		for (const kind of ['json', 'json_path']) {
			expect(coerceFilterValue(kind, 'null')).toEqual({ ok: false, error: 'Standalone JSON null is not supported. Use IS NULL or IS NOT NULL.' });
			expect(coerceFilterValue(kind, '"null"')).toEqual({ ok: true, value: 'null' });
			expect(coerceFilterValue(kind, '{"value":null}')).toEqual({ ok: true, value: { value: null } });
		}
	});

	it('passes through text and uuid as strings', () => {
		expect(coerceFilterValue('text', 'hello')).toEqual({ ok: true, value: 'hello' });
		expect(coerceFilterValue('uuid', 'abc-123')).toEqual({ ok: true, value: 'abc-123' });
		expect(coerceFilterValue('text_array', 'tag')).toEqual({ ok: true, value: 'tag' });
	});
});

describe('displayCell', () => {
	it('renders primitives and stringifies objects', () => {
		expect(displayCell(null)).toBe('');
		expect(displayCell(undefined)).toBe('');
		expect(displayCell('x')).toBe('x');
		expect(displayCell(5)).toBe('5');
		expect(displayCell(true)).toBe('true');
		expect(displayCell({ a: 1 })).toBe('{"a":1}');
		expect(displayCell([1, 2])).toBe('[1,2]');
	});
});
