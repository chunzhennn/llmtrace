import { describe, expect, it } from 'vitest';
import { requestToolHistory } from './request-tools';

const call = (id: string, name = 'add', args = '{"a":2,"b":3}') => ({
	role: 'assistant', content: null, tool_calls: [{ id, type: 'function', function: { name, arguments: args } }]
});
const result = (id: string, content: unknown) => ({ role: 'tool', tool_call_id: id, content });

describe('tool calls carried in a request', () => {
	it('shows the recorded input call and result even when no new tool call is generated', () => {
		const history = requestToolHistory({ messages: [
			{ role: 'user', content: 'Use add to calculate 2 + 3.' },
			call('call_y9Bfn9iXTD5rFQUJGm6SNblG'), result('call_y9Bfn9iXTD5rFQUJGm6SNblG', '5')
		] });
		expect(history).toEqual({ calls: [{ id: 'call_y9Bfn9iXTD5rFQUJGm6SNblG', name: 'add', arguments: '{"a":2,"b":3}', results: [{ callId: 'call_y9Bfn9iXTD5rFQUJGm6SNblG', content: '5', isError: false }] }], unmatchedResults: [] });
	});
	it('pairs parallel calls with the same name by exact ID, preserving multiple results', () => {
		const history = requestToolHistory({ messages: [call('a'), call('b'), result('b', 'second'), result('a', 'first'), result('b', 'extra')] });
		expect(history.calls.map(c => c.results.map(r => r.content))).toEqual([['first'], ['second', 'extra']]);
	});
	it('retains repeated calls and matches each result to the latest preceding call with its ID', () => {
		const history = requestToolHistory({ messages: [call('same'), result('same', 'one'), call('same'), result('same', 'two')] });
		expect(history.calls).toHaveLength(2);
		expect(history.calls.map(c => c.results.map(r => r.content))).toEqual([['one'], ['two']]);
	});
	it('keeps orphan results, including results before their call and results without an ID', () => {
		const history = requestToolHistory({ messages: [result('later', 'early'), call('later'), result('missing', 'orphan'), { role: 'tool', content: 'no ID' }] });
		expect(history.calls[0].results).toEqual([]);
		expect(history.unmatchedResults.map(r => r.content)).toEqual(['early', 'orphan', 'no ID']);
	});
	it('supports legacy function messages without confusing them with named calls using IDs', () => {
		const history = requestToolHistory({ messages: [
			{ role: 'assistant', function_call: { name: 'add', arguments: '{"a":1}' } }, call('modern'),
			{ role: 'function', name: 'add', content: 'legacy' }, result('modern', 'modern')
		] });
		expect(history.calls.map(c => c.results.map(r => r.content))).toEqual([['legacy'], ['modern']]);
	});
	it('reads Responses call IDs instead of item IDs and preserves custom tool input', () => {
		const history = requestToolHistory({ input: [
			{ type: 'function_call', id: 'fc_item', call_id: 'call_function', name: 'add', arguments: '{"a":2}' },
			{ type: 'custom_tool_call', id: 'ct_item', call_id: 'call_custom', name: 'script', input: 'print(5)\n' },
			{ type: 'custom_tool_call_output', call_id: 'call_custom', output: '5\n' },
			{ type: 'function_call_output', call_id: 'call_function', output: [{ type: 'input_text', text: '2' }] }
		] });
		expect(history.calls.map(c => c.id)).toEqual(['call_function', 'call_custom']);
		expect(history.calls[1].arguments).toBe('print(5)\n');
		expect(history.calls[1].results[0].content).toBe('5\n');
		expect(JSON.parse(history.calls[0].results[0].content)).toEqual([{ type: 'input_text', text: '2' }]);
	});
	it('keeps output-only Responses input when its call is in provider-managed history', () => {
		const history = requestToolHistory({ previous_response_id: 'earlier', input: { type: 'function_call_output', call_id: 'external', output: false } });
		expect(history).toEqual({ calls: [], unmatchedResults: [{ callId: 'external', content: 'false', isError: false }] });
	});
	it('supports Chat custom tools', () => {
		const history = requestToolHistory({ messages: [{ role: 'assistant', tool_calls: [{ id: 'custom', type: 'custom', custom: { name: 'script', input: 'print(5)' } }] }, result('custom', '5')] });
		expect(history.calls[0]).toMatchObject({ name: 'script', arguments: 'print(5)', results: [{ content: '5' }] });
	});
	it('preserves Anthropic inputs, server calls, structured results and error status', () => {
		const content = [{ type: 'text', text: 'Error details' }, { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'original-bytes' } }];
		const history = requestToolHistory({ messages: [
			{ role: 'assistant', content: [{ type: 'text', text: 'Checking' }, { type: 'tool_use', id: 'tool', name: 'inspect', input: { count: 0, enabled: false, empty: null } }, { type: 'server_tool_use', id: 'search', name: 'web_search', input: { query: 'query' } }] },
			{ role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tool', content, is_error: true }] },
			{ role: 'assistant', content: [{ type: 'web_search_tool_result', tool_use_id: 'search', content: [{ url: 'https://example.com', title: 'Example' }] }] }
		] });
		expect(JSON.parse(history.calls[0].arguments)).toEqual({ count: 0, enabled: false, empty: null });
		expect(JSON.parse(history.calls[0].results[0].content)).toEqual(content);
		expect(history.calls[0].results[0].isError).toBe(true);
		expect(history.calls[1].results[0].content).toContain('https://example.com');
	});
	it('preserves long arguments and results exactly, including line breaks and Unicode', () => {
		const long = 'sample 中文🙂\n'.repeat(20000) + 'THE END';
		const history = requestToolHistory({ messages: [call('long', 'echo', long), result('long', long)] });
		expect(history.calls[0].arguments).toBe(long);
		expect(history.calls[0].results[0].content).toBe(long);
	});
	it.each([0, false, null, ''])('retains a result containing %j', value => {
		const history = requestToolHistory({ messages: [call('id'), result('id', value)] });
		expect(history.calls[0].results[0].content).toBe(typeof value === 'string' ? value : JSON.stringify(value));
	});
	it('does not count declarations as invocations or inspect tool output as further calls', () => {
		const history = requestToolHistory({ tools: [{ type: 'function', function: { name: 'offered' } }], functions: [{ name: 'legacy' }], messages: [{ role: 'tool', tool_call_id: 'unknown', content: [{ type: 'tool_use', id: 'data', name: 'not-a-call', input: {} }] }] });
		expect(history.calls).toEqual([]);
		expect(history.unmatchedResults).toHaveLength(1);
	});
	it.each([undefined, null, 'text', [], { messages: [null, 3, { content: [null] }, { tool_calls: [null] }], input: 'text' }])('handles absent or invalid input shapes: %j', value => {
		expect(requestToolHistory(value)).toEqual({ calls: [], unmatchedResults: [] });
	});
});
