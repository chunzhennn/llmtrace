import { describe, expect, it } from 'vitest';
import { listReturnHref } from './navigation';

describe('listReturnHref', () => {
	it('preserves filters and pagination', () => {
		expect(listReturnHref('/ui/requests?model=a%2Fb&offset=50', '/ui/requests'))
			.toBe('/ui/requests?model=a%2Fb&offset=50');
	});
	it('rejects external URLs and unrelated routes', () => {
		for (const value of ['https://example.com/ui/requests', '//example.com/ui/requests', 'javascript:alert(1)', '/ui/admin/system', '/ui/requests/123', null]) {
			expect(listReturnHref(value, '/ui/requests')).toBe('/ui/requests');
		}
	});
});
