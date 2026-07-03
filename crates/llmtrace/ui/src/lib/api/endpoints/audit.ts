import { api, type QueryParams } from '../client';
import type { AuditEventsResponse, AuditSummary } from '../types';

export function listAuditEvents(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<AuditEventsResponse> {
	return api.get<AuditEventsResponse>('/audit-events', params, signal);
}

export function auditSummary(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<AuditSummary> {
	return api.get<AuditSummary>('/audit-events/summary', params, signal);
}
