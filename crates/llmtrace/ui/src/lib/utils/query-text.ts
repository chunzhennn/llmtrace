import type { DatasetSpec, FieldSpec, QueryFilter, QueryOp, QueryOrder, QuerySchema, StructuredQueryRequest } from '$lib/api/types';
import { isRfc3339Timestamp } from './query';

/** The editor language maps only to the existing structured query API. */
export class QueryTextError extends Error {
	constructor(message: string, public from: number, public to: number = from + 1) {
		super(message);
	}
}

export interface QueryToken {
	kind: 'word' | 'identifier' | 'string' | 'number' | 'symbol' | 'comment';
	text: string;
	from: number;
	to: number;
}

/** Keep offsets for editor diagnostics. Tolerant scanning is used by completion. */
export function queryTokens(text: string, tolerant = false, includeComments = false): QueryToken[] {
	const tokens: QueryToken[] = [];
	let i = 0;
	while (i < text.length) {
		if (/\s/.test(text[i])) { i++; continue; }
		if (text.startsWith('--', i)) {
			const from = i;
			const end = text.indexOf('\n', i);
			i = end < 0 ? text.length : end + 1;
			if (includeComments) tokens.push({ kind: 'comment', text: text.slice(from, i), from, to: i });
			continue;
		}
		if (text.startsWith('/*', i)) {
			const from = i;
			const end = text.indexOf('*/', i + 2);
			if (end < 0 && !tolerant) throw new QueryTextError('Close the comment with */.', i, text.length);
			i = end < 0 ? text.length : end + 2;
			if (includeComments) tokens.push({ kind: 'comment', text: text.slice(from, i), from, to: i });
			continue;
		}
		const from = i;
		if (text[i] === "'" || text[i] === '"') {
			const quote = text[i++];
			let value = '';
			let closed = false;
			while (i < text.length) {
				if (text[i] === quote) {
					if (text[i + 1] === quote) { value += quote; i += 2; }
					else { i++; closed = true; break; }
				} else value += text[i++];
			}
			if (!closed && !tolerant) throw new QueryTextError(`Close the ${quote === "'" ? 'string' : 'identifier'} with ${quote}.`, from, i);
			tokens.push({ kind: quote === "'" ? 'string' : 'identifier', text: value, from, to: i });
			continue;
		}
		const number = /^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(text.slice(i));
		if (number) {
			i += number[0].length;
			tokens.push({ kind: 'number', text: number[0], from, to: i });
			continue;
		}
		const word = /^[a-zA-Z_][a-zA-Z0-9_.-]*/.exec(text.slice(i));
		if (word) {
			i += word[0].length;
			tokens.push({ kind: 'word', text: word[0], from, to: i });
			continue;
		}
		const symbol = /^(?:>=|<=|!=|<>|[*,;=<>])/.exec(text.slice(i));
		if (!symbol && !tolerant) throw new QueryTextError(`Unexpected character ${JSON.stringify(text[i])}.`, i);
		i += symbol?.[0].length ?? 1;
		tokens.push({ kind: 'symbol', text: text.slice(from, i), from, to: i });
	}
	return tokens;
}

export const queryOperators: Record<QueryOp, string> = {
	eq: '=', ne: '!=', gt: '>', gte: '>=', lt: '<', lte: '<=',
	contains: 'CONTAINS', is_null: 'IS NULL', is_not_null: 'IS NOT NULL'
};

export function queryField(schema: QuerySchema, dataset: DatasetSpec, name: string): FieldSpec | undefined {
	const field = dataset.fields.find((f) => f.name === name);
	if (field) return field;
	const plugin = schema.plugin_metadata;
	if (dataset.name !== plugin.dataset || !name.startsWith(plugin.field_prefix)) return;
	const parts = name.slice(plugin.field_prefix.length).split('.');
	if (parts.length > plugin.max_segments || parts.some((p) =>
		!p || p.length > plugin.max_segment_bytes || !/^[a-zA-Z0-9_-]+$/.test(p))) return;
	return { name, filter_kind: plugin.filter_kind, operators: plugin.operators };
}

const byteLength = (value: string) => new TextEncoder().encode(value).length;

function safeJsonNumbers(value: unknown): boolean {
	if (typeof value === 'number') return Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value));
	if (value && typeof value === 'object') return Object.values(value).every(safeJsonNumbers);
	return true;
}

export function parseQueryText(text: string, schema: QuerySchema): StructuredQueryRequest {
	const tokens = queryTokens(text);
	let index = 0;
	const peek = () => tokens[index];
	const fail = (message: string, token = peek()): never => {
		throw new QueryTextError(message, token?.from ?? text.length, token?.to ?? text.length);
	};
	const keyword = (word: string) => {
		if (peek()?.kind === 'word' && peek().text.toUpperCase() === word) { index++; return true; }
		return false;
	};
	const symbol = (value: string) => {
		if (peek()?.kind === 'symbol' && peek().text === value) { index++; return true; }
		return false;
	};
	const expect = (word: string) => { if (!keyword(word)) fail(`Expected ${word}.`); };
	const identifier = () => {
		const token = peek();
		if (!token || !['word', 'identifier'].includes(token.kind)) fail('Expected a dataset or field name.');
		index++;
		return { ...token, text: token.kind === 'word' ? token.text.toLowerCase() : token.text };
	};
	expect('SELECT');
	const all = symbol('*');
	const selected: QueryToken[] = [];
	if (!all) { do { selected.push(identifier()); } while (symbol(',')); }
	expect('FROM');
	const source = identifier();
	const dataset = schema.datasets.find((d) => d.name === source.text);
	if (!dataset) fail(`Unknown dataset "${source.text}". Choose ${schema.datasets.map((d) => d.name).join(', ')}.`, source);
	const requireField = (token: QueryToken) => {
		const field = queryField(schema, dataset!, token.text);
		if (!field) fail(`Unknown field "${token.text}" in ${dataset!.name}.`, token);
		return field!;
	};
	selected.forEach(requireField);
	const fields = [...new Set(all ? dataset!.fields.map((f) => f.name) : selected.map((f) => f.text))];
	if (fields.length > schema.limits.max_fields) fail(`Select at most ${schema.limits.max_fields} fields.`, source);
	const filters: QueryFilter[] = [];
	const literal = (): unknown => {
		const token = peek();
		if (!token) fail('Expected a quoted string, number, TRUE, FALSE, or JSON literal.');
		if (keyword('JSON')) {
			const value = peek();
			if (value?.kind !== 'string') fail('Write JSON followed by a single-quoted JSON value.');
			index++;
			let parsed: unknown;
			try { parsed = JSON.parse(value.text); }
			catch { fail('Invalid JSON literal.', value); }
			if (parsed === null) fail('Standalone JSON null is not supported. Use IS NULL or IS NOT NULL.', value);
			if (!safeJsonNumbers(parsed)) fail('JSON numbers must be finite and integers must be within the safe integer range.', value);
			return parsed;
		}
		if (keyword('TRUE')) return true;
		if (keyword('FALSE')) return false;
		if (token.kind === 'string') { index++; return token.text; }
		if (token.kind === 'number') {
			index++;
			const value = Number(token.text);
			if (!Number.isFinite(value)) fail('Enter a finite number.', token);
			if (Number.isInteger(value) && !Number.isSafeInteger(value)) fail('Number exceeds the safe integer range.', token);
			return value;
		}
		fail('Quote text with single quotes. Use IS NULL / IS NOT NULL for missing values.', token);
	};
	if (keyword('WHERE')) {
		do {
			const name = identifier();
			const field = requireField(name);
			let op: QueryOp;
			if (keyword('IS')) { op = keyword('NOT') ? 'is_not_null' : 'is_null'; expect('NULL'); }
			else if (keyword('CONTAINS')) op = 'contains';
			else {
				const token = peek();
				const operators: Record<string, QueryOp> = { '=': 'eq', '!=': 'ne', '<>': 'ne', '>': 'gt', '>=': 'gte', '<': 'lt', '<=': 'lte' };
				const found = token?.kind === 'symbol' ? operators[token.text] : undefined;
				if (!found) fail('Expected =, !=, >, >=, <, <=, CONTAINS, or IS [NOT] NULL.');
				op = found!; index++;
			}
			if (!field.operators.includes(op)) fail(`${queryOperators[op]} is not supported for ${field.name}.`, name);
			if (op === 'is_null' || op === 'is_not_null') filters.push({ field: name.text, op });
			else {
				const token = peek();
				const value = literal();
				const kind = field.filter_kind;
				if (kind === 'int' && (typeof value !== 'number' || !Number.isSafeInteger(value))) fail(`${name.text} needs a safe integer.`, token);
				if (kind === 'bool' && typeof value !== 'boolean') fail(`${name.text} needs TRUE or FALSE.`, token);
				if (['text', 'text_array', 'uuid', 'timestamp'].includes(kind ?? '') && typeof value !== 'string') fail(`${name.text} needs a single-quoted string.`, token);
				if (kind === 'uuid' && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(String(value))) fail(`${name.text} needs a UUID.`, token);
				if (kind === 'timestamp' && !isRfc3339Timestamp(String(value))) fail(`${name.text} needs an RFC3339 timestamp with a timezone, e.g. '2026-09-22T00:00:00+08:00'.`, token);
				const json = kind === 'json' || kind === 'json_path';
				const size = byteLength(json ? JSON.stringify(value) : String(value));
				if (size > (json ? schema.limits.max_json_value_bytes : schema.limits.max_string_value_bytes)) fail('Filter value exceeds the allowed size.', token);
				filters.push({ field: name.text, op, value });
			}
		} while (keyword('AND'));
	}
	if (filters.length > schema.limits.max_filters) fail(`Use at most ${schema.limits.max_filters} filters.`);
	const order: QueryOrder[] = [];
	if (keyword('ORDER')) {
		expect('BY');
		do {
			const name = identifier(); requireField(name);
			const direction = keyword('DESC') ? 'desc' : (keyword('ASC'), 'asc');
			order.push({ field: name.text, direction });
		} while (symbol(','));
	}
	if (order.length > schema.limits.max_order_by) fail(`Use at most ${schema.limits.max_order_by} sort keys.`);
	let limit = 100;
	if (keyword('LIMIT')) {
		const token = peek();
		if (token?.kind !== 'number') fail('LIMIT needs an integer.');
		limit = Number(token.text); index++;
		if (!Number.isSafeInteger(limit) || limit < 1 || limit > schema.limits.max_limit) fail(`LIMIT must be between 1 and ${schema.limits.max_limit}.`, token);
	}
	symbol(';');
	if (peek()) fail('Use one SELECT query with AND filters, ORDER BY and LIMIT. Joins, OR, expressions and multiple statements are not supported.');
	return { dataset: dataset!.name, fields, filters, order_by: order.length ? order : dataset!.default_order.map((o) => ({ ...o })), limit };
}

const quote = (value: string) => `'${value.replaceAll("'", "''")}'`;
const name = (value: string) => /^[a-z_][a-z0-9_]*$/.test(value) ? value : `"${value.replaceAll('"', '""')}"`;

export function formatQueryText(request: StructuredQueryRequest, schema: QuerySchema): string {
	const dataset = schema.datasets.find((d) => d.name === request.dataset);
	if (!dataset) throw new Error('Choose a dataset.');
	const fields = request.fields ?? dataset.default_fields;
	const lines = [`SELECT ${fields.map(name).join(', ')}`, `FROM ${name(request.dataset)}`];
	const filters = (request.filters ?? []).map((filter) => {
		const op = queryOperators[filter.op];
		if (filter.op === 'is_null' || filter.op === 'is_not_null') return `${name(filter.field)} ${op}`;
		const value = filter.value;
		let literal: string;
		if (typeof value === 'string') literal = quote(value);
		else if (typeof value === 'boolean') literal = value ? 'TRUE' : 'FALSE';
		else if (typeof value === 'number') literal = String(value);
		else literal = `JSON ${quote(JSON.stringify(value))}`;
		return `${name(filter.field)} ${op} ${literal}`;
	});
	if (filters.length) lines.push(`WHERE ${filters.join('\n  AND ')}`);
	const order = request.order_by?.length ? request.order_by : dataset.default_order;
	if (order.length) lines.push(`ORDER BY ${order.map((o) => `${name(o.field)} ${(o.direction ?? 'desc').toUpperCase()}`).join(', ')}`);
	lines.push(`LIMIT ${request.limit ?? 100};`);
	return lines.join('\n');
}

export function queryErrorMessage(error: unknown, text: string): string {
	if (!(error instanceof QueryTextError)) return error instanceof Error ? error.message : 'Invalid query.';
	const before = text.slice(0, error.from).split('\n');
	return `Line ${before.length}, column ${before.at(-1)!.length + 1}: ${error.message}`;
}
