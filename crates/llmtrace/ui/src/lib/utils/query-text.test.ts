import { describe, expect, it } from 'vitest';
import type { FieldSpec, QueryOp, QuerySchema, StructuredQueryRequest } from '$lib/api/types';
import { formatQueryText, parseQueryText, queryErrorMessage, QueryTextError } from './query-text';
import { querySuggestions } from './query-completion';
import { coerceFilterValue } from './query';

const comparisons: QueryOp[] = ['eq', 'ne', 'gt', 'gte', 'lt', 'lte', 'is_null', 'is_not_null'];
const field = (name: string, filter_kind: string | null, operators: QueryOp[] = comparisons): FieldSpec => ({ name, filter_kind, operators });
const schema: QuerySchema = {
	limits: { max_fields: 64, max_filters: 32, max_order_by: 8, max_limit: 500, max_string_value_bytes: 4096, max_json_value_bytes: 16384 },
	sort_directions: ['asc', 'desc'],
	plugin_metadata: { dataset: 'requests', field_prefix: 'plugin_metadata.', filter_kind: 'json_path', operators: ['eq', 'ne', 'contains', 'is_null', 'is_not_null'], max_segments: 16, max_segment_bytes: 64, segment_pattern: '[A-Za-z0-9_-]+' },
	datasets: [
		{ name: 'requests', default_fields: ['id', 'model', 'status'], default_order: [{ field: 'started_at', direction: 'desc' }], fields: [
			field('id', 'uuid'), field('model', 'text', [...comparisons, 'contains']), field('status', 'int'),
			field('started_at', 'timestamp'), field('usage_complete', 'bool', ['eq', 'ne', 'is_null', 'is_not_null']),
			field('tags', 'text_array', ['contains', 'is_null', 'is_not_null']),
			field('plugin_metadata', 'json', ['eq', 'ne', 'contains', 'is_null', 'is_not_null']),
			field('display_only', null, [])
		] },
		{ name: 'sessions', default_fields: ['id', 'first_seen'], default_order: [{ field: 'last_seen', direction: 'desc' }], fields: [field('id', 'uuid'), field('first_seen', 'timestamp'), field('last_seen', 'timestamp')] }
	]
};
const parse = (text: string) => parseQueryText(text, schema);

describe('query statement parsing', () => {
	it('translates projection, typed filters, sorting and limit into the existing API shape', () => {
		expect(parse("SELECT id, model FROM requests WHERE status >= 400 AND usage_complete = TRUE ORDER BY started_at DESC, id ASC LIMIT 25;")).toEqual({
			dataset: 'requests', fields: ['id', 'model'], filters: [{ field: 'status', op: 'gte', value: 400 }, { field: 'usage_complete', op: 'eq', value: true }],
			order_by: [{ field: 'started_at', direction: 'desc' }, { field: 'id', direction: 'asc' }], limit: 25
		});
	});
	it('accepts mixed-case keywords and comments without treating quoted content as SQL', () => {
		const q = parse("-- select a model\nSeLeCt MODEL /* comment */ FrOm REQUESTS WHERE model = 'x''; DROP TABLE users; --' LIMIT 1; -- done");
		expect(q.fields).toEqual(['model']);
		expect(q.filters?.[0].value).toBe("x'; DROP TABLE users; --");
	});
	it('expands star into all schema fields and uses dataset defaults for omitted sort/limit', () => {
		const q = parse('SELECT * FROM sessions');
		expect(q.fields).toEqual(['id', 'first_seen', 'last_seen']);
		expect(q.order_by).toEqual([{ field: 'last_seen', direction: 'desc' }]);
		expect(q.limit).toBe(100);
	});
	it('uses SQL ascending order when ORDER BY omits a direction', () => {
		expect(parse('SELECT id FROM requests ORDER BY id').order_by).toEqual([{ field: 'id', direction: 'asc' }]);
	});
	it('supports null tests, literal text containment and array membership', () => {
		expect(parse("SELECT id FROM requests WHERE status IS NOT NULL AND model CONTAINS 'gpt_%' AND tags CONTAINS 'review'").filters).toEqual([
			{ field: 'status', op: 'is_not_null' }, { field: 'model', op: 'contains', value: 'gpt_%' }, { field: 'tags', op: 'contains', value: 'review' }
		]);
	});
	it('preserves timestamp offsets and microseconds through Builder conversion', () => {
		const time = '2026-09-22T13:48:49.123456+08:00';
		const q = parse(`SELECT id FROM requests WHERE started_at >= '${time}'`);
		expect(q.filters?.[0].value).toBe(time);
		expect(coerceFilterValue('timestamp', time)).toEqual({ ok: true, value: time });
		expect(parse(formatQueryText(q, schema))).toEqual(q);
	});
	it('round-trips nested JSON nulls, JSON strings and case-sensitive plugin paths', () => {
		const q = parse(`SELECT id, "plugin_metadata.My-plugin.team" FROM requests
			WHERE plugin_metadata CONTAINS JSON '{"team":"research","n":null}'
			AND "plugin_metadata.My-plugin.team" = '123'
			AND plugin_metadata.example.missing IS NULL
			ORDER BY "plugin_metadata.My-plugin.team" DESC LIMIT 10`);
		expect(q.filters?.[0].value).toEqual({ team: 'research', n: null });
		expect(q.filters?.[1].value).toBe('123');
		expect(q.filters?.[2]).toEqual({ field: 'plugin_metadata.example.missing', op: 'is_null' });
		expect(parse(formatQueryText(q, schema))).toEqual(q);
	});
	it.each(['=', '!=', 'CONTAINS'])('rejects standalone JSON null for %s before execution', (op) => {
		expect(() => parse(`SELECT id FROM requests WHERE plugin_metadata ${op} JSON 'null'`)).toThrow('Use IS NULL or IS NOT NULL');
	});
	it('formats Builder defaults without changing their meaning', () => {
		const q: StructuredQueryRequest = { dataset: 'requests', filters: [{ field: 'model', op: 'eq', value: "O'Reilly" }] };
		const compiled = parse(formatQueryText(q, schema));
		expect(compiled.fields).toEqual(schema.datasets[0].default_fields);
		expect(compiled.filters).toEqual(q.filters);
		expect(compiled.order_by).toEqual(schema.datasets[0].default_order);
	});
	it.each([
		'SELECT id FROM users', 'SELECT password FROM requests',
		'SELECT id FROM sessions WHERE plugin_metadata.identity.team = 1',
		'SELECT plugin_metadata.bad..path FROM requests',
		'SELECT id FROM requests; DELETE FROM requests',
		'SELECT id FROM requests WHERE status = 200 OR status = 400',
		'SELECT id FROM requests JOIN sessions ON id = id',
		'SELECT count(*) FROM requests', 'DELETE FROM requests',
		'SELECT id FROM requests LIMIT 501', 'SELECT id FROM requests LIMIT 1.5',
		'SELECT id FROM requests LIMIT 0', 'SELECT id FROM requests LIMIT -2',
		'SELECT id FROM requests WHERE status = 1.1',
		'SELECT id FROM requests WHERE status = 9007199254740993',
		"SELECT id FROM requests WHERE status = '200'",
		'SELECT id FROM requests WHERE model = 200',
		'SELECT id FROM requests WHERE status CONTAINS 2',
		'SELECT id FROM requests WHERE usage_complete > TRUE',
		'SELECT id FROM requests WHERE display_only IS NULL',
		"SELECT id FROM requests WHERE id = 'not-a-uuid'",
		"SELECT id FROM requests WHERE started_at > 'yesterday'",
		"SELECT id FROM requests WHERE started_at > '2026-09-22T10:00:00'",
		"SELECT id FROM requests WHERE started_at > '2026-02-30T10:00:00Z'",
		"SELECT id FROM requests WHERE started_at > '2025-02-29T10:00:00Z'",
		"SELECT id FROM requests WHERE started_at > '2026-09-22T24:00:00Z'",
		"SELECT id FROM requests WHERE plugin_metadata = JSON '{'",
		"SELECT id FROM requests WHERE plugin_metadata = JSON '{\"n\":9007199254740993}'",
		"SELECT id FROM requests WHERE plugin_metadata = JSON '[1e400]'",
		'SELECT id FROM requests WHERE plugin_metadata.example.count = 9007199254740993',
		'SELECT id FROM requests WHERE model = NULL',
		"SELECT id FROM requests WHERE model = 'unclosed",
		'SELECT id FROM requests /* unclosed',
		'SELECT id FROM requests WHERE'
	])('rejects unsupported or invalid input without sending SQL: %s', (text) => {
		expect(() => parse(text)).toThrow(QueryTextError);
	});
	it('enforces schema limits and reports the failing location', () => {
		expect(() => parseQueryText('SELECT id, model FROM requests', { ...schema, limits: { ...schema.limits, max_fields: 1 } })).toThrow('at most 1');
		expect(() => parseQueryText("SELECT id FROM requests WHERE model = '中文'", { ...schema, limits: { ...schema.limits, max_string_value_bytes: 5 } })).toThrow('size');
		try { parse('SELECT id\nFROM unknown'); } catch (error) {
			expect(queryErrorMessage(error, 'SELECT id\nFROM unknown')).toMatch(/^Line 2, column 6: Unknown dataset/);
		}
	});
});

describe('query completion', () => {
	const suggestions = (text: string) => {
		const at = text.indexOf('|');
		return querySuggestions(text.replace('|', ''), at < 0 ? text.length : at, schema).options;
	};
	const labels = (text: string) => suggestions(text).map((s) => s.label);
	it('offers datasets after FROM', () => {
		expect(labels('SELECT id FROM req|')).toEqual(['requests', 'sessions']);
	});
	it('uses the selected dataset even when completing before FROM', () => {
		expect(labels('SELECT fir| FROM sessions')).toContain('first_seen');
		expect(labels('SELECT fir| FROM sessions')).not.toContain('model');
	});
	it('offers schema-approved operators for the current field', () => {
		expect(labels('SELECT id FROM requests WHERE usage_complete |')).toEqual(['=', '!=', 'IS NULL', 'IS NOT NULL']);
		expect(labels('SELECT id FROM requests WHERE model CON|')).toContain('CONTAINS');
	});
	it('offers boolean values, clause keywords and ordering directions', () => {
		expect(labels('SELECT id FROM requests WHERE usage_complete = |')).toEqual(['TRUE', 'FALSE']);
		expect(labels('SELECT id FROM requests WHERE status = 200 |')).toEqual(['AND', 'ORDER BY', 'LIMIT']);
		expect(labels('SELECT id FROM requests ORDER BY id |')).toEqual(['ASC', 'DESC', 'LIMIT']);
	});
		it('does not suggest completions inside strings, comments or quoted identifiers', () => {
		expect(labels("SELECT id FROM requests WHERE model = 'hel|")).toEqual([]);
		expect(labels('SELECT id FROM requests -- comment|')).toEqual([]);
		expect(labels('SELECT id FROM requests /* comment|')).toEqual([]);
		expect(labels('SELECT "plugin_metadata.foo|')).toEqual([]);
	});
	it('continues completion after strings containing comment markers and completed comments', () => {
		expect(labels("SELECT id FROM requests WHERE model = 'x--y/*z' |" )).toContain('AND');
		expect(labels('SELECT id FROM requests -- comment\nWHERE |')).toContain('model');
		expect(labels('SELECT id FROM requests /* comment */ WHERE |')).toContain('model');
	});
	it('offers plugin paths only for the supported dataset', () => {
		expect(labels('SELECT id FROM requests WHERE |')).toContain('plugin_metadata.');
		expect(labels('SELECT id FROM sessions WHERE |')).not.toContain('plugin_metadata.');
	});
});
