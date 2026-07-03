import { redirect } from '@sveltejs/kit';
import { base } from '$app/paths';
import { auth } from '$lib/state/auth.svelte';

// Client-side auth guard for the authenticated app shell. Runs on every
// navigation into the (app) group; redirects to the login page when there is
// no valid session.
export const load = async () => {
	if (auth.status === 'unknown') {
		await auth.refresh();
	}
	if (auth.status !== 'authenticated') {
		redirect(307, `${base}/login`);
	}
	return {};
};
