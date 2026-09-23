import { toast } from 'svelte-sonner';

// Application durations; Sonner owns queueing, IDs, dismissal and timer cleanup.
export const toasts = {
	success: (message: string) => toast.success(message, { duration: 5000 }),
	error: (message: string) => toast.error(message, { duration: 8000 }),
	info: (message: string) => toast.info(message, { duration: 5000 })
};
