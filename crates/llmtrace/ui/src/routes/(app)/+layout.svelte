<script lang="ts">
	import { page } from '$app/state';
	import { base } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import { auth } from '$lib/state/auth.svelte';
	import { theme } from '$lib/state/theme.svelte';

	let { children } = $props();

	let mobileOpen = $state(false);

	interface NavItem {
		href: string;
		label: string;
		icon: string;
	}
	interface NavGroup {
		label: string;
		items: NavItem[];
	}

	const groups: NavGroup[] = [
		{
			label: 'Observe',
			items: [
				{ href: '/', label: 'Overview', icon: 'home' },
				{ href: '/requests', label: 'Requests', icon: 'list' },
				{ href: '/sessions', label: 'Sessions', icon: 'messages' },
				{ href: '/analytics', label: 'Analytics', icon: 'chart' },
				{ href: '/query', label: 'Query', icon: 'search' }
			]
		},
		{
			label: 'Admin',
			items: [
				{ href: '/audit', label: 'Audit Log', icon: 'shield' },
				{ href: '/admin/ui-sessions', label: 'UI Sessions', icon: 'key' },
				{ href: '/admin/plugins', label: 'Plugins', icon: 'box' },
				{ href: '/admin/system', label: 'System', icon: 'settings' },
				{ href: '/admin/redaction', label: 'Redaction', icon: 'eye-off' }
			]
		}
	];

	const current = $derived(page.url.pathname);

	function isActive(href: string): boolean {
		const full = href === '/' ? base : `${base}${href}`;
		if (href === '/') return current === base || current === `${base}/`;
		return current === full || current.startsWith(`${full}/`);
	}

	function href(item: NavItem): string {
		return item.href === '/' ? `${base}/` : `${base}${item.href}`;
	}
</script>

<div class="flex min-h-screen">
	<!-- Sidebar -->
	<aside
		class="fixed inset-y-0 left-0 z-40 flex w-64 flex-col border-r transition-transform md:translate-x-0 {mobileOpen
			? 'translate-x-0'
			: '-translate-x-full'}"
		style="background-color: var(--color-surface); border-color: var(--color-border);"
	>
		<div class="flex h-14 items-center gap-2 px-4" style="border-bottom: 1px solid var(--color-border);">
			<div
				class="flex h-8 w-8 items-center justify-center rounded-lg"
				style="background-color: var(--color-brand); color: var(--color-brand-fg);"
			>
				<Icon name="activity" size={18} />
			</div>
			<span class="font-semibold">llmtrace</span>
		</div>

		<nav class="flex-1 overflow-y-auto px-3 py-4">
			{#each groups as group (group.label)}
				<div class="mb-5">
					<div class="text-fg-muted mb-1 px-2 text-[0.7rem] font-semibold uppercase tracking-wider">
						{group.label}
					</div>
					{#each group.items as item (item.href)}
						<a
							href={href(item)}
							class="mb-0.5 flex items-center gap-3 rounded-lg px-2 py-2 text-sm transition-colors"
							style={isActive(item.href)
								? 'background-color: color-mix(in srgb, var(--color-brand) 15%, transparent); color: var(--color-brand); font-weight: 600;'
								: 'color: var(--color-fg);'}
							onclick={() => (mobileOpen = false)}
							aria-current={isActive(item.href) ? 'page' : undefined}
						>
							<Icon name={item.icon} size={17} />
							{item.label}
						</a>
					{/each}
				</div>
			{/each}
		</nav>
	</aside>

	{#if mobileOpen}
		<button
			type="button"
			class="fixed inset-0 z-30 bg-black/40 md:hidden"
			aria-label="Close menu"
			onclick={() => (mobileOpen = false)}
		></button>
	{/if}

	<!-- Main -->
	<div class="flex min-w-0 flex-1 flex-col md:pl-64">
		<header
			class="sticky top-0 z-20 flex h-14 items-center gap-3 border-b px-4"
			style="background-color: color-mix(in srgb, var(--color-surface) 85%, transparent); border-color: var(--color-border); backdrop-filter: blur(8px);"
		>
			<button
				type="button"
				class="btn !p-2 md:hidden"
				aria-label="Open menu"
				onclick={() => (mobileOpen = true)}
			>
				<Icon name="menu" />
			</button>

			<div class="flex-1"></div>

			<button
				type="button"
				class="btn !p-2"
				aria-label="Toggle theme"
				title="Toggle theme"
				onclick={() => theme.toggle()}
			>
				<Icon name={theme.dark ? 'sun' : 'moon'} />
			</button>

			<div class="flex items-center gap-2 pl-1">
				<div class="hidden text-right sm:block">
					<div class="text-sm font-medium leading-tight">
						{auth.user?.display_name ?? auth.user?.user_id ?? 'User'}
					</div>
					<div class="text-fg-muted text-xs leading-tight">
						{auth.user?.login_method ?? ''}
					</div>
				</div>
				<button
					type="button"
					class="btn !p-2"
					aria-label="Log out"
					title="Log out"
					onclick={() => auth.logout()}
				>
					<Icon name="log-out" />
				</button>
			</div>
		</header>

		<main class="min-w-0 flex-1 p-4 md:p-6">
			{@render children()}
		</main>
	</div>
</div>
