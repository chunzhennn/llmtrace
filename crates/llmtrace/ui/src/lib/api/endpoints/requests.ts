import { api, type QueryParams } from '../client';
import type {
	RequestDetail,
	RequestFacets,
	RequestListResponse,
	RecentErrorsResponse,
	SlowRequestsResponse
} from '../types';

export function listRequests(
	params: QueryParams,
	signal?: AbortSignal
): Promise<RequestListResponse> {
	return api.get<RequestListResponse>('/requests', params, signal);
}

export function getRequest(id: string, signal?: AbortSignal): Promise<RequestDetail> {
	return api.get<RequestDetail>(`/requests/${id}`, undefined, signal);
}

export function requestFacets(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<RequestFacets> {
	return api.get<RequestFacets>('/requests/facets', params, signal);
}

export function recentErrors(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<RecentErrorsResponse> {
	return api.get<RecentErrorsResponse>('/requests/recent-errors', params, signal);
}

export function slowRequests(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<SlowRequestsResponse> {
	return api.get<SlowRequestsResponse>('/requests/slow', params, signal);
}
