import { api } from '../client';
import type { MeResponse, LoginMethods } from '../types';

export function methods(): Promise<LoginMethods> {
	return api.probe<LoginMethods>('/auth/methods');
}

export function me(signal?: AbortSignal): Promise<MeResponse> {
	return api.probe<MeResponse>('/auth/me', signal);
}

export function login(username: string, password: string): Promise<{ ok: boolean }> {
	return api.postProbe<{ ok: boolean }>('/auth/login', { username, password });
}

export function logout(): Promise<{ ok: boolean }> {
	return api.post<{ ok: boolean }>('/auth/logout');
}

/** Full-page navigation target that begins the OAuth login flow. */
export const OAUTH_START_URL = '/api/auth/oauth/start';
