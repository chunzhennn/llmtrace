import { goto } from '$app/navigation';
import { base } from '$app/paths';
import { setUnauthorizedHandler } from '../api/client';
import * as authApi from '../api/endpoints/auth';
import type { MeUser } from '../api/types';

type AuthStatus = 'unknown' | 'authenticated' | 'unauthenticated';

class AuthStore {
	status = $state<AuthStatus>('unknown');
	user = $state<MeUser | null>(null);

	async refresh(): Promise<AuthStatus> {
		try {
			const result = await authApi.me();
			if (result.authenticated && result.user) {
				this.user = result.user;
				this.status = 'authenticated';
			} else {
				this.user = null;
				this.status = 'unauthenticated';
			}
		} catch {
			this.user = null;
			this.status = 'unauthenticated';
		}
		return this.status;
	}

	markAuthenticated(user: MeUser): void {
		this.user = user;
		this.status = 'authenticated';
	}

	async logout(): Promise<void> {
		try {
			await authApi.logout();
		} catch {
			// even if the request fails, treat the client as logged out
		}
		this.user = null;
		this.status = 'unauthenticated';
		await goto(`${base}/login`);
	}

	handleUnauthorized(): void {
		this.user = null;
		this.status = 'unauthenticated';
		void goto(`${base}/login`);
	}
}

export const auth = new AuthStore();

// Bridge the API client's 401 handling into the auth store without a circular
// import (the client only knows about a registered callback).
setUnauthorizedHandler(() => auth.handleUnauthorized());
