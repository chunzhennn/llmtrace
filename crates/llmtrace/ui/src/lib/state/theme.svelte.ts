import { browser } from '$app/environment';

const STORAGE_KEY = 'llmtrace:theme';

class ThemeStore {
	dark = $state(true);

	init(): void {
		if (!browser) return;
		try {
			const stored = localStorage.getItem(STORAGE_KEY);
			this.dark = stored ? stored === 'dark' : true;
		} catch {
			this.dark = true;
		}
		this.apply();
	}

	toggle(): void {
		this.dark = !this.dark;
		if (browser) {
			try {
				localStorage.setItem(STORAGE_KEY, this.dark ? 'dark' : 'light');
			} catch {
				// ignore storage errors
			}
			this.apply();
		}
	}

	private apply(): void {
		document.documentElement.classList.toggle('dark', this.dark);
	}
}

export const theme = new ThemeStore();
