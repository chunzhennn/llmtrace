import type { QueryFilter, QuerySchema, StructuredQueryRequest, StructuredQueryResult } from '$lib/api/types';
import { validatedQuery } from './query-text';

export interface TagSearchInput {
	tag: string;
	op: 'eq' | 'contains' | 'is_not_null';
	value: string;
	role?: string;
	since?: string;
	until?: string;
}

/** Server-discovered fields and one validator serve Sessions and Query. */
export function promptTagQuery(schema: QuerySchema, input: TagSearchInput, sessionId?: string): StructuredQueryRequest {
	const filters: QueryFilter[] = [{ field: 'prompt_tag.name', op: 'eq', value: input.tag }, { field: 'prompt_tag.value', op: input.op, ...(input.op === 'is_not_null' ? {} : { value: input.value }) }];
	if (input.role) filters.push({ field: 'prompt_tag.role', op: 'eq', value: input.role });
	if (input.since) filters.push({ field: 'prompt_tag.observed_at', op: 'gte', value: input.since });
	if (input.until) filters.push({ field: 'prompt_tag.observed_at', op: 'lte', value: input.until });
	if (sessionId) {
		const mapping: Record<string, string> = { 'prompt_tag.name': 'tag_name', 'prompt_tag.value': 'tag_value', 'prompt_tag.role': 'role', 'prompt_tag.observed_at': 'observed_at' };
		return validatedQuery({ dataset: 'prompt_tags', filters: [{ field: 'session_id', op: 'eq', value: sessionId }, ...filters.map(f => ({ ...f, field: mapping[f.field] }))], limit: 50 }, schema);
	}
	return validatedQuery({ dataset: 'sessions', filters, limit: 50 }, schema);
}

/** Live pages can move; hidden cursor row IDs also work when ID is unselected. */
export function mergeQueryPage(previous: StructuredQueryResult, next: StructuredQueryResult): StructuredQueryResult {
	const ids = previous.page?.row_ids;
	const nextIds = next.page?.row_ids;
	if (!ids?.length || ids.length !== previous.rows.length || nextIds?.length !== next.rows.length) {
		return { ...next, rows: [...previous.rows, ...next.rows] };
	}
	const seen = new Set(ids);
	const rows = [...previous.rows];
	const rowIds = [...ids];
	next.rows.forEach((row, i) => { if (!seen.has(nextIds[i])) { seen.add(nextIds[i]); rows.push(row); rowIds.push(nextIds[i]); } });
	return { ...next, rows, page: { ...next.page!, row_ids: rowIds } };
}
