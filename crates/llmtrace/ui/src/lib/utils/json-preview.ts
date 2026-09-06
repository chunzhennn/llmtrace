export const JSON_PREVIEW_CHARS = 128 * 1024;

export function jsonText(value: unknown): string {
	try { return JSON.stringify(value, null, 2) ?? String(value); }
	catch { return String(value); }
}

/** Bound traversal and string allocation before pretty-printing large traces. */
export function jsonPreview(value: unknown, limit = JSON_PREVIEW_CHARS) {
	let remaining = Math.max(0, limit);
	let nodes = 4096;
	let truncated = false;
	const seen = new WeakSet<object>();
	function visit(item: unknown, depth: number): unknown {
		if (remaining <= 0 || nodes-- <= 0 || depth > 24) {
			truncated = true;
			return '…';
		}
		if (typeof item === 'string') {
			if (item.length <= remaining) { remaining -= item.length; return item; }
			truncated = true;
			let end = remaining;
			if (end > 0 && /[\uD800-\uDBFF]/.test(item[end - 1])) end--;
			remaining = 0;
			return item.slice(0, end) + '…';
		}
		if (item === null || typeof item !== 'object') { remaining -= 16; return item; }
		if (seen.has(item)) return '[Circular]';
		seen.add(item);
		const result: unknown[] | Record<string, unknown> = Array.isArray(item) ? [] : Object.create(null);
		let count = 0;
		for (const key in item) {
			if (!Object.prototype.hasOwnProperty.call(item, key)) continue;
			if (remaining <= 0 || nodes <= 0 || count++ >= 256) {
				truncated = true;
				if (Array.isArray(result)) result.push('…');
				else result['…'] = 'preview truncated';
				break;
			}
			if (key.length + 8 > remaining) {
				truncated = true;
				if (Array.isArray(result)) result.push('…');
				else result['…'] = 'preview truncated';
				break;
			}
			remaining -= key.length + 8;
			const child = visit((item as Record<string, unknown>)[key], depth + 1);
			if (Array.isArray(result)) result.push(child);
			else result[key] = child;
		}
		seen.delete(item);
		return result;
	}
	const text = jsonText(visit(value, 0));
	return { text: text.slice(0, limit), truncated: truncated || text.length > limit };
}
