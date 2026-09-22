import { describe, expect, it } from 'vitest';
import { TranscriptHistory } from './dedupe';
import { parseRequest } from './parse';
import { chat, record, sse } from './test-fixtures';
import type { ContextNode, TranscriptMessage } from './types';
const u = (content: string) => ({ role: 'user', content });
const a = (content: string) => ({ role: 'assistant', content });

describe('conservative transcript context reuse', () => {
	it('folds only a complete matching prefix and keeps every newly generated answer', () => {
		const history = new TranscriptHistory();
		const first = record({ messages: [u('hello')] }, chat('answer'), 1);
		const second = record({ messages: [u('hello'), a('answer'), u('next')] }, chat('answer'), 2);
		const parsed = parseRequest(first);
		parsed.notices.push('Request display notice unrelated to completeness.');
		history.add(first, parsed);
		const result = history.add(second, parseRequest(second));
		expect(result.block.reusedCount).toBe(2);
		expect(result.block.messages.map(m => m.content)).toEqual(['next', 'answer']);
	});
	it.each([
		['previous_response_id', (n: number) => ({ previous_response_id: `resp_${n}`, input: 'continue' })],
		['conversation', () => ({ conversation: 'conv_1', input: 'continue' })],
		['item_reference', () => ({ input: [{ type: 'item_reference', id: 'msg_1' }, u('continue')] })]
	])('keeps incremental input with %s visible without learning a conversation prefix', (_kind, input) => {
		const history = new TranscriptHistory();
		for (const n of [1, 2]) {
			const r = record(input(n), chat('reply'), n);
			const parsed = parseRequest(r);
			const result = history.add(r, parsed);
			expect(parsed.context).toBe('provider-managed');
			expect(result.block.reusedCount).toBe(0);
			expect(result.block.messages.map(m => m.content)).toEqual([...parsed.input, ...parsed.output].map(m => m.content));
			expect(result.nodes).toEqual([]);
		}
		const snapshot = record({ messages: [u('continue')] }, chat('reply'), 3);
		expect(history.add(snapshot, parseRequest(snapshot)).block.reusedCount).toBe(0);
		const continuation = record(input(4), chat('reply'), 4);
		expect(history.add(continuation, parseRequest(continuation)).block.reusedCount).toBe(0);
	});
	it('does not erase a repeated question in a new turn or identical tool outputs at different positions', () => {
		const history = new TranscriptHistory();
		const first = record({ messages: [u('again')] }, chat('same'), 1);
		history.add(first, parseRequest(first));
		const second = record({ messages: [u('again'), a('same'), u('again'), { role: 'tool', tool_call_id: 'a', content: 'ok' }, { role: 'tool', tool_call_id: 'b', content: 'ok' }] }, chat('same'), 2);
		const result = history.add(second, parseRequest(second));
		expect(result.block.messages.map(m => m.content.split('\n')[0])).toEqual(['again', 'ok', 'ok', 'same']);
	});
	it('retains changed names, IDs, arguments, whitespace, images and unknown metadata', () => {
		for (const [before, after] of [
			[{ role: 'tool', tool_call_id: 'a', content: 'ok' }, { role: 'tool', tool_call_id: 'b', content: 'ok' }],
			[{ role: 'user', name: 'A', content: 'same' }, { role: 'user', name: 'B', content: 'same' }],
			[u('same'), u('same ')],
			[{ role: 'user', content: [{ type: 'image_url', image_url: { url: 'a' } }] }, { role: 'user', content: [{ type: 'image_url', image_url: { url: 'b' } }] }],
			[{ role: 'assistant', tool_calls: [{ id: 'c', function: { arguments: '{"a":1}' } }] }, { role: 'assistant', tool_calls: [{ id: 'c', function: { arguments: '{"a":2}' } }] }],
			[{ role: 'user', custom: 'before', content: 'same' }, { role: 'user', custom: 'after', content: 'same' }]
		]) {
			const history = new TranscriptHistory();
			const r1 = record({ messages: [before] }, chat('answer'), 1); history.add(r1, parseRequest(r1));
			const r2 = record({ messages: [after] }, chat('answer'), 2);
			expect(history.add(r2, parseRequest(r2)).block.reusedCount).toBe(0);
		}
	});
	it('keeps diverging branches and can later continue either exact branch', () => {
		const history = new TranscriptHistory();
		for (const [i, messages, reply, expected] of [
			[1, [u('root')], 'A', 0],
			[2, [u('root')], 'B', 1],
			[3, [u('root'), a('A'), u('branch A')], 'A2', 2],
			[4, [u('root'), a('B'), u('branch B')], 'B2', 2],
			[5, [u('root'), a('A'), u('branch A'), a('A2'), u('more')], 'A3', 4]
		] as const) {
			const r = record({ messages }, chat(reply), i);
			expect(history.add(r, parseRequest(r)).block.reusedCount).toBe(expected);
		}
	});
	it('does not guess at sliding windows or remove non-contiguous matches after a difference', () => {
		const history = new TranscriptHistory();
		const r1 = record({ messages: [u('first'), a('second'), u('third')] }, chat('fourth'), 1); history.add(r1, parseRequest(r1));
		const r2 = record({ messages: [a('second'), u('third'), a('fourth')] }, chat('fifth'), 2);
		expect(history.add(r2, parseRequest(r2)).block.reusedCount).toBe(0);
		const r3 = record({ messages: [u('first'), a('changed'), u('third')] }, chat('fourth'), 3);
		const result = history.add(r3, parseRequest(r3));
		expect(result.block.reusedCount).toBe(1);
		expect(result.block.messages.map(m => m.content)).toEqual(['changed', 'third', 'fourth']);
	});
	it('treats multiple model choices as alternative paths, never one concatenated history', () => {
		const history = new TranscriptHistory();
		const r1 = record({ messages: [u('root')] }, { choices: [{ message: a('A') }, { message: a('B') }] });
		history.add(r1, parseRequest(r1));
		const r2 = record({ messages: [u('root'), a('A'), a('B')] }, chat('next'), 2);
		expect(history.add(r2, parseRequest(r2)).block.reusedCount).toBe(2);
	});
	it('never uses incomplete captures as evidence to suppress a later message', () => {
		const history = new TranscriptHistory();
		const r1 = record({ messages: [u('root')] }, chat('partial')); r1.request_body.truncated = true;
		const parsed = parseRequest(r1);
		parsed.notices = [];
		history.add(r1, parsed);
		const r2 = record({ messages: [u('root'), a('partial')] }, chat('next'), 2);
		expect(history.add(r2, parseRequest(r2)).block.reusedCount).toBe(0);
	});
	it('does not learn output from a malformed stream even with a terminal event', () => {
		const history = new TranscriptHistory();
		const first = record({ messages: [u('root')] }, sse(
			{ choices: [{ delta: { role: 'assistant', content: 'partial' } }] }, '{broken', '[DONE]'
		), 1, true);
		const parsed = parseRequest(first);
		expect(parsed.outputComplete).toBe(false);
		expect(parsed.output.some(m => m.content === '{broken')).toBe(true);
		history.add(first, parsed);
		const second = record({ messages: [u('root'), a('partial')] }, chat('next'), 2);
		expect(history.add(second, parseRequest(second)).block.reusedCount).toBe(1);
	});
	it('stores repeated context as references, with exact expansion, over 300 growing requests', () => {
		const history = new TranscriptHistory();
		const nodes = new Map<number, ContextNode>();
		const visible = new Map<string, TranscriptMessage>();
		const input = [];
		let lastNode = 0;
		for (let i = 1; i <= 300; i++) {
			input.push(u(`question ${i}`));
			const r = record({ messages: input }, chat(`answer ${i}`), i);
			const result = history.add(r, parseRequest(r));
			expect(result.block.reusedCount).toBe((i - 1) * 2);
			expect(result.block.messages).toHaveLength(2);
			for (const n of result.nodes) nodes.set(n.id, n);
			for (const m of result.block.messages) visible.set(m.id, m);
			lastNode = result.block.reusedNode;
			input.push(a(`answer ${i}`));
		}
		expect(nodes.size).toBe(600); expect(visible.size).toBe(600);
		const expanded = [];
		while (lastNode) { const node = nodes.get(lastNode)!; expanded.push(visible.get(node.messageId)!.content); lastNode = node.parent; }
		expect(expanded.reverse()).toEqual(input.slice(0, 598).map(m => m.content));
	});
});

it('matches tool-only streamed assistant messages echoed with content null', () => {
    const history = new TranscriptHistory();
    const tool = { id: 'call1', type: 'function', function: { name: 'add', arguments: '{"a":2}' } };
    const first = record({ messages: [u('call the tool')] }, { choices: [{ message: { role: 'assistant', tool_calls: [tool] } }] });
    history.add(first, parseRequest(first));
    const second = record({ messages: [u('call the tool'), { role: 'assistant', content: null, tool_calls: [tool] }, { role: 'tool', tool_call_id: 'call1', content: '2' }] }, chat('done'), 2);
    const result = history.add(second, parseRequest(second));
    expect(result.block.reusedCount).toBe(2);
    expect(result.block.messages).toHaveLength(2);
});
