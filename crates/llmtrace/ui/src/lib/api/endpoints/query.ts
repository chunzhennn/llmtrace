import { api } from '../client';
import type { NoteContext, RequestPromptIndex, QuerySchema, StructuredQueryRequest, StructuredQueryResult } from '../types';

export function querySchema(signal?: AbortSignal): Promise<QuerySchema> {
	return api.get<QuerySchema>('/query/schema', undefined, signal);
}

export function runQuery(
	request: StructuredQueryRequest,
	signal?: AbortSignal
): Promise<StructuredQueryResult> {
	return api.post<StructuredQueryResult>('/query', request, undefined, signal);
}

export function noteContext(id: string, signal?: AbortSignal): Promise<NoteContext> {
	return api.get<NoteContext>(`/prompt-tags/${encodeURIComponent(id)}/context`, undefined, signal);
}

export function requestPromptIndex(id: string, signal?: AbortSignal): Promise<RequestPromptIndex> {
	return api.get<RequestPromptIndex>(`/requests/${encodeURIComponent(id)}/prompt-index`, undefined, signal);
}
