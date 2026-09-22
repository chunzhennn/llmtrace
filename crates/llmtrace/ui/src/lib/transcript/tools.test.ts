import { describe, expect, it } from 'vitest';
import { toolDeclarations, describeTools } from './tools';
import { parseRequest } from './parse';
import { TranscriptHistory } from './dedupe';
import { chat, record } from './test-fixtures';

const schema = { type: 'object', properties: { a: { type: 'integer', description: 'First integer' }, b: { type: 'integer' } }, required: ['a', 'b'], additionalProperties: false };
const definition = { name: 'add', description: 'Add two integers', parameters: schema };

describe('provided tool definitions', () => {
	it.each([
		['Chat Completions', { tools: [{ type: 'function', function: definition }] }],
		['Responses', { tools: [{ type: 'function', ...definition }] }],
		['Messages', { tools: [{ name: definition.name, description: definition.description, input_schema: schema }] }],
		['legacy functions', { functions: [definition] }]
	])('reads names and descriptions for %s and preserves the complete original schema', (_kind, body) => {
		const declarations = toolDeclarations(body);
		const tools = describeTools(declarations);
		expect(tools).toHaveLength(1);
		expect(tools[0].name).toBe('add'); expect(tools[0].description).toBe('Add two integers');
		expect(JSON.parse(tools[0].definition)).toEqual(declarations[0].value);
		expect(tools[0].definition).toContain('First integer');
		expect(tools[0].definition).toContain('additionalProperties');
	});
	it('includes tools that were offered but never called, including both declaration styles', () => {
		const parsed = parseRequest(record({ messages: [{ role: 'user', content: 'No tools needed' }], tools: [{ type: 'function', function: definition }], functions: [{ ...definition, name: 'unused' }] }, chat('hello')));
		expect(describeTools(parsed.tools!).map(t => t.name)).toEqual(['add', 'unused']);
	});
	it('keeps built-in tools and unknown definitions without inventing descriptions', () => {
		const declarations = toolDeclarations({ tools: [{ type: 'web_search', search_context_size: 'low' }, { type: 'custom', name: 'grammar_tool', description: 'Use grammar', format: { type: 'grammar', definition: 'root = "hello"' } }, { unexpected: { content: 'preserve me' } }, null] });
		const tools = describeTools(declarations);
		expect(tools.map(t => t.name)).toEqual(['web_search', 'grammar_tool', 'Unnamed tool 3', 'Unnamed tool 4']);
		expect(tools[0].description).toBeNull(); expect(tools[2].description).toBeNull();
		expect(tools.map(t => JSON.parse(t.definition))).toEqual(declarations.map(t => t.value));
	});
	it('does not truncate long descriptions, strict mode, nested schemas or unknown options', () => {
		const long = '描述🙂 '.repeat(10000) + 'END';
		const tool = { type: 'function', function: { ...definition, description: long, strict: true, parameters: { ...schema, $defs: { extra: { enum: [long] } } } }, extension: { special: true } };
		const output = describeTools(toolDeclarations({ tools: [tool] }))[0];
		expect(output.description).toBe(long); expect(JSON.parse(output.definition)).toEqual(tool);
	});
	it('does not infer declarations from generated calls or from another request', () => {
		const missing = record({ tools: [definition] }, chat('reply'));
		missing.request_body.status = 'missing'; missing.request_body.data = null;
		expect(parseRequest(missing).tools).toBeNull();
		const malformed = record({}, chat('reply')); malformed.request_body.data = '{"tools":';
		expect(parseRequest(malformed).tools).toBeNull();
		expect(parseRequest(record({ messages: [] }, { choices: [{ message: { tool_calls: [{ function: definition }] } }] })).tools).toEqual([]);
	});
});

describe('tool definitions across a session', () => {
	it('shares repeated definitions but preserves changed descriptions and schemas even with the same name', () => {
		const history = new TranscriptHistory();
		const add = (tools: unknown[], i: number) => { const r = record({ messages: [], tools }, chat('answer'), i); return history.add(r, parseRequest(r)); };
		const first = add([{ type: 'function', function: definition }], 1);
		const repeated = add([{ function: { parameters: schema, description: definition.description, name: 'add' }, type: 'function' }], 2);
		expect(first.block.toolsChanged).toBe(true); expect(first.toolSet?.tools).toHaveLength(1);
		expect(repeated.toolSet).toBeUndefined(); expect(repeated.block.toolSetId).toBe(first.block.toolSetId); expect(repeated.block.toolsChanged).toBe(false);
		const changedDescription = add([{ type: 'function', function: { ...definition, description: 'New description' } }], 3);
		expect(changedDescription.block.toolsChanged).toBe(true); expect(changedDescription.block.toolSetId).not.toBe(first.block.toolSetId);
		const changedSchema = add([{ type: 'function', function: { ...definition, parameters: { type: 'string' } } }], 4);
		expect(changedSchema.block.toolsChanged).toBe(true); expect(changedSchema.toolSet?.tools[0].definition).toContain('string');
		const restored = add([{ type: 'function', function: definition }], 5);
		expect(restored.toolSet).toBeUndefined(); expect(restored.block.toolsChanged).toBe(true); expect(restored.block.toolSetId).toBe(first.block.toolSetId);
	});
	it('clears the available set when a later request omits tools or has unavailable input', () => {
		const history = new TranscriptHistory();
		const first = record({ tools: [definition], messages: [] }, chat('answer'), 1); history.add(first, parseRequest(first));
		const second = record({ messages: [] }, chat('answer'), 2);
		const empty = history.add(second, parseRequest(second)); expect(empty.block.toolsChanged).toBe(true); expect(empty.toolSet?.tools).toEqual([]);
		const third = record({ tools: [definition] }, chat('answer'), 3); third.request_body.data = null; third.request_body.status = 'unreadable';
		const unreadable = history.add(third, parseRequest(third)); expect(unreadable.block.toolSetId).toBeNull(); expect(unreadable.toolSet).toBeUndefined();
	});
	it('transfers one set rather than copying a large schema for every repeated request', () => {
		const history = new TranscriptHistory(); let transferred = 0; const ids = new Set<number | null>();
		const tool = { ...definition, description: 'Long description '.repeat(4096) };
		for (let i = 1; i <= 100; i++) {
			const r = record({ tools: [tool], messages: [] }, chat('reply'), i);
			const event = history.add(r, parseRequest(r));
			if (event.toolSet) transferred++; ids.add(event.block.toolSetId);
		}
		expect(transferred).toBe(1); expect(ids.size).toBe(1);
	});
});
