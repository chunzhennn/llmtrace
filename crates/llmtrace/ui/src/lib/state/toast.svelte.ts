export type ToastKind = 'success' | 'error' | 'info';

export interface Toast {
	id: number;
	kind: ToastKind;
	message: string;
}

class ToastStore {
	items = $state<Toast[]>([]);
	#nextId = 1;

	push(kind: ToastKind, message: string, timeoutMs = 5000): number {
		const id = this.#nextId++;
		this.items = [...this.items, { id, kind, message }];
		if (timeoutMs > 0) {
			setTimeout(() => this.dismiss(id), timeoutMs);
		}
		return id;
	}

	success(message: string): number {
		return this.push('success', message);
	}

	error(message: string): number {
		return this.push('error', message, 8000);
	}

	info(message: string): number {
		return this.push('info', message);
	}

	dismiss(id: number): void {
		this.items = this.items.filter((toast) => toast.id !== id);
	}
}

export const toasts = new ToastStore();
