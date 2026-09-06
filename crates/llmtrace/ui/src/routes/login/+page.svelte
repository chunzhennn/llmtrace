<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import { ApiError } from '$lib/api/client';
	import * as authApi from '$lib/api/endpoints/auth';
	import { auth } from '$lib/state/auth.svelte';
	import { theme } from '$lib/state/theme.svelte';
	import type { LoginMethods } from '$lib/api/types';

	let username = $state('');
	let password = $state('');
	let submitting = $state(false);
	let error = $state<string | null>(null);
	let checking = $state(true);
	let methods = $state<LoginMethods | null>(null);

	onMount(async () => {
		const status = await auth.refresh();
		if (status === 'authenticated') {
			await goto(`${base}/`);
			return;
		}
		try {
			methods = await authApi.methods();
		} catch {
			error = 'Unable to load sign-in options. Refresh the page to try again.';
		}
		checking = false;
	});

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (submitting) return;
		error = null;
		submitting = true;
		try {
			await authApi.login(username, password);
			await auth.refresh();
			await goto(`${base}/`);
		} catch (err) {
			if (err instanceof ApiError) {
				if (err.status === 429 && err.retryAfterSecs) {
					error = `Too many attempts. Try again in ${err.retryAfterSecs}s.`;
				} else {
					error = err.message;
				}
			} else {
				error = 'Login failed. Please try again.';
			}
		} finally {
			submitting = false;
		}
	}

	function startOauth() {
		window.location.href = authApi.OAUTH_START_URL;
	}
</script>

<svelte:head>
	<title>Sign in · llmtrace</title>
</svelte:head>

<div class="relative flex min-h-screen items-center justify-center p-4">
	<button
		type="button"
		class="btn absolute right-4 top-4 !p-2"
		aria-label="Toggle theme"
		onclick={() => theme.toggle()}
	>
		<Icon name={theme.dark ? 'sun' : 'moon'} />
	</button>

	{#if checking}
		<div class="text-fg-muted flex items-center gap-2 text-sm">
			<Icon name="refresh" class="animate-spin" />
			Checking session…
		</div>
	{:else}
		<div class="card w-full max-w-sm p-6 shadow-lg">
			<div class="mb-6 flex items-center gap-3">
				<div
					class="flex h-10 w-10 items-center justify-center rounded-lg"
					style="background-color: var(--color-brand); color: var(--color-brand-fg);"
				>
					<Icon name="activity" size={22} />
				</div>
				<div>
					<h1 class="text-lg font-semibold leading-tight">llmtrace</h1>
					<p class="text-fg-muted text-xs">LLM proxy observability</p>
				</div>
			</div>

			{#if methods?.local}
			<form onsubmit={submit} class="flex flex-col gap-4">
				<div>
					<label class="label" for="username">Username</label>
					<input
						id="username"
						class="input"
						type="text"
						autocomplete="username"
						bind:value={username}
						required
					/>
				</div>
				<div>
					<label class="label" for="password">Password</label>
					<input
						id="password"
						class="input"
						type="password"
						autocomplete="current-password"
						bind:value={password}
						required
					/>
				</div>

				{#if error}
					<div
						class="flex items-start gap-2 rounded-lg p-2 text-sm"
						style="background-color: color-mix(in srgb, var(--color-danger) 12%, transparent); color: var(--color-danger);"
						role="alert"
					>
						<Icon name="alert" size={16} class="mt-0.5 shrink-0" />
						<span>{error}</span>
					</div>
				{/if}

				<button class="btn btn-brand w-full" type="submit" disabled={submitting}>
					{#if submitting}
						<Icon name="refresh" size={16} class="animate-spin" />
						Signing in…
					{:else}
						Sign in
					{/if}
				</button>
			</form>
			{/if}

			{#if methods?.oauth}
			{#if methods.local}
			<div class="my-4 flex items-center gap-3">
				<div class="h-px flex-1" style="background-color: var(--color-border);"></div>
				<span class="text-fg-muted text-xs uppercase tracking-wide">or</span>
				<div class="h-px flex-1" style="background-color: var(--color-border);"></div>
			</div>
			{/if}

			<button class="btn w-full" type="button" onclick={startOauth}>
				<Icon name="external" size={16} />
				Continue with SSO
			</button>
			{/if}
			{#if !methods?.local && error}
				<p class="mt-3 text-sm" role="alert">{error}</p>
			{:else if methods && !methods.local && !methods.oauth}
				<p class="text-fg-muted text-sm">No sign-in method is configured. Contact your administrator.</p>
			{/if}
		</div>
	{/if}
</div>
