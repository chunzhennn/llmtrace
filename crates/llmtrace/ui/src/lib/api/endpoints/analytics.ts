import { api, type QueryParams } from '../client';
import type {
	ApiKeyUsage,
	ErrorSummary,
	LatencySummary,
	ModelUsage,
	Stats,
	UpstreamHealth,
	UsageSummary,
	UsageTimeseries,
	UserUsage
} from '../types';

export function stats(signal?: AbortSignal): Promise<Stats> {
	return api.get<Stats>('/stats', undefined, signal);
}

export function usageSummary(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<UsageSummary> {
	return api.get<UsageSummary>('/usage/summary', params, signal);
}

export function usageTimeseries(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<UsageTimeseries> {
	return api.get<UsageTimeseries>('/usage/timeseries', params, signal);
}

export function latencySummary(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<LatencySummary> {
	return api.get<LatencySummary>('/usage/latency', params, signal);
}

export function apiKeyUsage(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<ApiKeyUsage> {
	return api.get<ApiKeyUsage>('/usage/api-keys', params, signal);
}

export function modelUsage(params?: QueryParams, signal?: AbortSignal): Promise<ModelUsage> {
	return api.get<ModelUsage>('/usage/models', params, signal);
}

export function userUsage(params?: QueryParams, signal?: AbortSignal): Promise<UserUsage> {
	return api.get<UserUsage>('/usage/users', params, signal);
}

export function upstreamHealth(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<UpstreamHealth> {
	return api.get<UpstreamHealth>('/usage/upstreams', params, signal);
}

export function errorSummary(
	params?: QueryParams,
	signal?: AbortSignal
): Promise<ErrorSummary> {
	return api.get<ErrorSummary>('/usage/errors', params, signal);
}
