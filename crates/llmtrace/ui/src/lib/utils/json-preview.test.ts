import { describe, expect, it } from 'vitest';
import { jsonPreview, jsonText, JSON_PREVIEW_CHARS } from './json-preview';

describe('JSON previews', () => {
	it('preserves complete small JSON', () => {
		const value = { a: [1, 'hello', null, true] };
		expect(jsonPreview(value)).toEqual({ text: JSON.stringify(value, null, 2), truncated: false });
	});
	it('bounds multi-megabyte bodies while retaining complete copy text on demand', () => {
		const value = { request_body: 'x'.repeat(8 * 1024 * 1024) };
		const preview = jsonPreview(value);
		expect(preview.text.length).toBeLessThanOrEqual(JSON_PREVIEW_CHARS);
		expect(preview.truncated).toBe(true);
		expect(jsonText(value).length).toBeGreaterThan(8 * 1024 * 1024);
	});
	it('stops traversing wide arrays', () => {
		const value = Array.from({ length: 10000 }, () => 'hello');
		Object.defineProperty(value, 1000, { get() { throw new Error('unbounded traversal'); } });
		expect(jsonPreview(value).truncated).toBe(true);
	});
	it('skips oversized field names without visiting their values', () => {
		const value = Object.defineProperty({}, 'x'.repeat(1024 * 1024), {
			enumerable: true,
			get() { throw new Error('oversized field visited'); }
		});
		const preview = jsonPreview(value);
		expect(preview.text.length).toBeLessThanOrEqual(JSON_PREVIEW_CHARS);
		expect(preview.truncated).toBe(true);
	});
	it('bounds deep structures and handles cycles', () => {
		const value: Record<string, unknown> = {};
		value.self = value;
		expect(jsonPreview(value).text).toContain('[Circular]');
		let deep: unknown = 1;
		for (let i = 0; i < 100; i++) deep = { child: deep };
		expect(jsonPreview(deep).truncated).toBe(true);
	});
});
