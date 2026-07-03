import { api } from '../client';
import type { QuerySchema, StructuredQueryRequest, StructuredQueryResult } from '../types';

export function querySchema(signal?: AbortSignal): Promise<QuerySchema> {
	return api.get<QuerySchema>('/query/schema', undefined, signal);
}

export function runQuery(
	request: StructuredQueryRequest,
	signal?: AbortSignal
): Promise<StructuredQueryResult> {
	return api.post<StructuredQueryResult>('/query', request, undefined, signal);
}
