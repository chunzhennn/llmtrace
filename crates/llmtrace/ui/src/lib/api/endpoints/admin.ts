import { api, type QueryParams } from '../client';
import type {
	PluginsResponse,
	RedactionPreview,
	RedactionPreviewRequest,
	RetentionStatus,
	RuntimeConfig,
	SecurityPosture,
	StorageSummary,
	UiSession,
	UiSessionsResponse
} from '../types';

export function plugins(signal?: AbortSignal): Promise<PluginsResponse> {
	return api.get<PluginsResponse>('/plugins', undefined, signal);
}

export function runtimeConfig(signal?: AbortSignal): Promise<RuntimeConfig> {
	return api.get<RuntimeConfig>('/config', undefined, signal);
}

export function securityPosture(signal?: AbortSignal): Promise<SecurityPosture> {
	return api.get<SecurityPosture>('/security/posture', undefined, signal);
}

export function retentionStatus(signal?: AbortSignal): Promise<RetentionStatus> {
	return api.get<RetentionStatus>('/retention/status', undefined, signal);
}

export function storageSummary(signal?: AbortSignal): Promise<StorageSummary> {
	return api.get<StorageSummary>('/storage/summary', undefined, signal);
}

export function listUiSessions(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<UiSessionsResponse> {
	return api.get<UiSessionsResponse>('/ui-sessions', params, signal);
}

export function revokeUiSession(
	sessionHash: string
): Promise<{ revoked: boolean; session: UiSession }> {
	return api.del<{ revoked: boolean; session: UiSession }>(`/ui-sessions/${sessionHash}`);
}

export function redactionPreview(
	body: RedactionPreviewRequest,
	signal?: AbortSignal
): Promise<RedactionPreview> {
	return api.post<RedactionPreview>('/redaction/preview', body, undefined, signal);
}
