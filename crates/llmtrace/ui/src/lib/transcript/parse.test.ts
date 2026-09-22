import { describe, expect, it } from 'vitest';
import { canonical, parseRequest } from './parse';
import { chat, record, sse } from './test-fixtures';

const user = (content: unknown) => ({ role: 'user', content });
const text = (parsed: ReturnType<typeof parseRequest>) => [...parsed.input, ...parsed.output].map(m => m.content).join('\n');

describe('complete captured transcript parsing', () => {
	it('keeps long Unicode content, all 200 input messages, and long tool arguments', () => {
		const long = '中文🙂\n'.repeat(12000) + 'THE END';
		const input = Array.from({ length: 200 }, (_, i) => user(i === 199 ? long : String(i)));
		const parsed = parseRequest(record({ messages: input }, { choices: [{ message: { role: 'assistant', content: null, tool_calls: [{ id: 'call1', type: 'function', function: { name: 'write', arguments: long } }] } }] }));
		expect(parsed.input).toHaveLength(200);
		expect(parsed.input[199].content).toBe(long);
		expect(parsed.output[0].content).toContain(JSON.stringify(long));
		expect(parsed.notices).toEqual([]);
	});
	it('keeps multimodal blocks, reasoning, refusals, tool IDs, names and unknown message fields', () => {
		const parsed = parseRequest(record({ messages: [{ role: 'tool', tool_call_id: 'call1', name: 'run', content: [{ type: 'image_url', image_url: { url: 'data:image/png;base64,AAAA' } }, { type: 'text', text: 'caption', extra: 'retain' }] }] }, { choices: [{ message: { role: 'assistant', content: '', reasoning: 'think', refusal: 'refuse', custom: { secret: 'retained' } } }] }));
		for (const retained of ['call1', 'data:image/png;base64,AAAA', 'caption', 'retain', 'think', 'refuse', 'retained']) expect(text(parsed)).toContain(retained);
	});
	it('preserves every alternative choice as a separate labeled output', () => {
		const parsed = parseRequest(record({ messages: [] }, { choices: [{ index: 0, message: { content: 'first' } }, { index: 1, message: { content: 'second' } }] }));
		expect(parsed.output.map(m => m.content)).toEqual(['first', 'second']);
		expect(parsed.output.map(m => m.label)).toEqual(['Choice 1', 'Choice 2']);
		expect(parsed.outputBranches).toHaveLength(2);
	});
	it('normalizes only equivalent simple text representations and empty optional fields', () => {
		const parsed = parseRequest(record({ messages: [user('hello')] }, { choices: [{ message: { role: 'user', content: [{ type: 'output_text', text: 'hello' }], refusal: null, annotations: [] } }] }));
		expect(parsed.input[0].identity).toBe(parsed.output[0].identity);
		expect(canonical({ b: 1, a: 2 })).toBe(canonical({ a: 2, b: 1 }));
		expect(parseRequest(record({ messages: [user('hello ')] }, chat('hello'))).input[0].identity).not.toBe(parsed.input[0].identity);
	});
	it('keeps Responses instructions, input strings, calls, results, references, reasoning and output blocks', () => {
		const parsed = parseRequest(record({ instructions: 'system', input: ['hello', { type: 'function_call', call_id: 'c1', name: 'run', arguments: '{}' }, { type: 'function_call_output', call_id: 'c1', output: 'result' }, { type: 'item_reference', id: 'msg_prev' }] }, { output: [{ type: 'reasoning', summary: [{ type: 'summary_text', text: 'thought' }], encrypted_content: 'opaque' }, { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'answer' }] }, { type: 'image_generation_call', result: 'image-data' }] }));
		for (const retained of ['system', 'hello', 'c1', 'result', 'msg_prev', 'thought', 'opaque', 'answer', 'image-data']) expect(text(parsed)).toContain(retained);
	});
	it('keeps Anthropic system blocks, thinking signatures, tool use and tool results', () => {
		const parsed = parseRequest(record({ system: [{ type: 'text', text: 'system', cache_control: { type: 'ephemeral' } }], messages: [user([{ type: 'tool_result', tool_use_id: 't1', content: 'tool result' }])] }, { type: 'message', role: 'assistant', content: [{ type: 'thinking', thinking: 'thought', signature: 'signature' }, { type: 'tool_use', id: 't2', name: 'run', input: { code: 'long code' } }] }));
		for (const retained of ['system', 'ephemeral', 't1', 'tool result', 'thought', 'signature', 't2', 'long code']) expect(text(parsed)).toContain(retained);
	});
	it('assembles all chat delta choices, tools and reasoning without bounded preview parsers', () => {
		const parsed = parseRequest(record({ messages: [] }, sse(
			{ choices: [{ index: 0, delta: { role: 'assistant', content: 'hel', reasoning: 'think ', tool_calls: [{ index: 0, id: 'call1', type: 'function', function: { name: 'run', arguments: '{"x":' } }] } }, { index: 1, delta: { content: 'alternative' } }] },
			{ choices: [{ index: 0, delta: { content: 'lo', reasoning: 'more', tool_calls: [{ index: 0, function: { arguments: '42}' } }] } }] }, '[DONE]'
		), 1, true));
		for (const retained of ['hello', 'think more', 'call1', '{\\"x\\":42}', 'alternative']) expect(text(parsed)).toContain(retained);
		expect(parsed.notices).toEqual([]);
		expect(parsed.outputComplete).toBe(true);
	});
	it('assembles Anthropic text, thinking, signatures and unbounded input JSON deltas', () => {
		const parsed = parseRequest(record({ messages: [] }, sse(
			{ type: 'message_start', message: { role: 'assistant', content: [] } },
			{ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } },
			{ type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'thought' } },
			{ type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'signed' } },
			{ type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'c1', name: 'run', input: {} } },
			{ type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{"a":' } },
			{ type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '1}' } },
			{ type: 'message_stop' }
		), 1, true));
		for (const retained of ['thought', 'signed', 'c1', '"a": 1']) expect(text(parsed)).toContain(retained);
		expect(parsed.outputComplete).toBe(true);
	});
	it('uses final Responses output without duplicating streamed text or tool arguments', () => {
		const item = { id: 'msg1', type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'full answer' }] };
		const parsed = parseRequest(record({ input: 'question' }, sse(
			{ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'full ' },
			{ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'answer' },
			{ type: 'response.output_item.done', output_index: 0, item },
			{ type: 'response.completed', response: { output: [item] } }
		), 1, true));
		expect(parsed.output).toHaveLength(1);
		expect(parsed.output[0].content.match(/full answer/g)).toHaveLength(1);
		expect(parsed.outputComplete).toBe(true);
	});
	it('keeps partial Responses output, reasoning, tool arguments and unknown stream events', () => {
		const parsed = parseRequest(record({ input: [] }, sse(
			{ type: 'response.reasoning_summary_part.done', output_index: 0, summary_index: 0, part: { type: 'summary_text', text: 'reasoning' } },
			{ type: 'response.function_call_arguments.delta', output_index: 1, item_id: 'call1', delta: '{"incomplete":' },
			{ type: 'provider.new_event', content: 'unknown output' }
		), 1, true));
		for (const retained of ['reasoning', 'call1', 'incomplete', 'unknown output']) expect(text(parsed)).toContain(retained);
		expect(parsed.notices.join(' ')).toContain('no terminal event');
		expect(parsed.outputComplete).toBe(false);
	});
	it('keeps malformed events, CRLF and multiline SSE data', () => {
		const parsed = parseRequest(record({ messages: [] }, 'data: {"choices":\r\ndata: [{"delta":{"content":"ok"}}]}\r\n\r\ndata: {broken\r\n\r\ndata: [DONE]\r\n\r\n', 1, true));
		expect(text(parsed)).toContain('ok'); expect(text(parsed)).toContain('{broken');
	});
	it('reports missing, truncated and binary bodies without substituting previews', () => {
		const r = record({ messages: [user('full')] }, chat('answer'));
		r.request_body = { status: 'missing', encoding: null, data: null, captured_bytes: null, truncated: false };
		r.response_body = { status: 'available', encoding: 'base64', data: 'AAEC/w==', captured_bytes: 4, truncated: true };
		const parsed = parseRequest(r);
		expect(text(parsed)).toContain('AAEC/w==');
		expect(parsed.notices.join(' ')).toContain('Request body is missing');
		expect(parsed.notices.join(' ')).toContain('truncated during capture');
		expect(parsed.inputComplete).toBe(false);
		expect(parsed.outputComplete).toBe(false);
	});
	it('shows unrecognized captured text and errors rather than omitting the request', () => {
		const r = record({}, { error: { message: 'failure' } }); r.request_body.data = '{invalid request';
		const parsed = parseRequest(r);
		expect(text(parsed)).toContain('{invalid request'); expect(text(parsed)).toContain('failure');
	});
});

it('preserves unexpected delta keys without mutating object prototypes or allocating huge sparse arrays', () => {
	const delta = JSON.parse('{"__proto__":{"transcriptPolluted":"yes"},"content":"hello"}');
	const parsed = parseRequest(record({ messages: [] }, sse(
		{ choices: [{ index: 0, message: { role: 'assistant', content: '' } }] },
		{ choices: [{ index: 0, delta }] },
		{ choices: [{ index: 0, delta: { tool_calls: [{ index: 999999999, function: { arguments: 'still retained' } }] } }] }, '[DONE]'
	), 1, true));
	expect(Object.getOwnPropertyDescriptor(Object.prototype, 'transcriptPolluted')).toBeUndefined();
	expect(text(parsed)).toContain('hello'); expect(text(parsed)).toContain('still retained');
});
