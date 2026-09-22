export async function copyText(text: string): Promise<void> {
	try {
		if (navigator.clipboard) {
			await navigator.clipboard.writeText(text);
			return;
		}
	} catch {
		// Fall back when clipboard permissions are unavailable.
	}

	// HTTP deployments do not expose the Clipboard API.
	const previousFocus = document.activeElement;
	const textarea = document.createElement('textarea');
	textarea.value = text;
	textarea.readOnly = true;
	textarea.style.position = 'fixed';
	textarea.style.opacity = '0';
	document.body.append(textarea);
	try {
		textarea.select();
		if (!document.execCommand('copy')) {
			throw new Error('Clipboard copy failed');
		}
	} finally {
		textarea.remove();
		if (previousFocus instanceof HTMLElement) previousFocus.focus({ preventScroll: true });
	}
}
