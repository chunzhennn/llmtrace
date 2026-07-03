import { api, type QueryParams } from '../client';
import type {
	Paginated,
	SessionDetail,
	SessionRequestsResponse,
	SessionSummary
} from '../types';

export function listSessions(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<Paginated<SessionSummary>> {
	return api.get<Paginated<SessionSummary>>('/sessions', params, signal);
}

export function getSession(
	id: string,
	params?: QueryParams,
	signal?: AbortSignal
): Promise<SessionDetail> {
	return api.get<SessionDetail>(`/sessions/${id}`, params, signal);
}

export function listSessionRequests(
	id: string,
	params?: QueryParams,
	signal?: AbortSignal
): Promise<SessionRequestsResponse> {
	return api.get<SessionRequestsResponse>(`/sessions/${id}/requests`, params, signal);
}
