import type { SessionExportBody } from '$lib/api/types';
import type { ExportRequest, ParsedMessage, ParsedRequest } from './types';
import { toolDeclarations } from './tools';

type Obj = Record<string, unknown>;
const object = (value: unknown): value is Obj => value !== null && typeof value === 'object' && !Array.isArray(value);
const array = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
const has = (value: Obj, key: string) => Object.hasOwn(value, key);

interface ParsedOutput {
	messages: ParsedMessage[];
	branches: ParsedMessage[][];
	complete: boolean;
}

/** Sort object keys only. Text, whitespace, array order, tool IDs and arguments remain exact. */
export function canonical(value: unknown): string {
	return JSON.stringify(value, (_key, item) => object(item)
		? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item);
}
function normalizedContent(value: unknown): unknown {
	if (value == null) return null;
	if (typeof value === 'string') return [{ type: 'text', text: value }];
	if (Array.isArray(value)) return value.map(part => object(part)
		&& ['text', 'input_text', 'output_text'].includes(String(part.type))
		&& typeof part.text === 'string' && Object.keys(part).every(key => key === 'type' || key === 'text')
		? { type: 'text', text: part.text } : part);
	return value;
}
function displayContent(value: unknown): string {
	if (typeof value === 'string') return value;
	if (Array.isArray(value)) return value.map(part => {
		if (typeof part === 'string') return part;
		if (object(part) && ['text', 'input_text', 'output_text'].includes(String(part.type))
			&& typeof part.text === 'string' && Object.keys(part).every(key => key === 'type' || key === 'text')) return part.text;
		return JSON.stringify(part, null, 2);
	}).join('\n\n');
	return value == null ? '' : JSON.stringify(value, null, 2);
}
function message(value: unknown, fallback = 'user'): ParsedMessage {
	if (!object(value)) value = { role: fallback, content: value };
	const raw = value as Obj;
	const role = typeof raw.role === 'string' ? raw.role : fallback;
	// Only empty optional chat fields are equivalent to their absence. Unknown fields are preserved.
	const extra = Object.fromEntries(Object.entries(raw).filter(([key, v]) => key !== 'role' && key !== 'content'
		&& !(key === 'type' && v === 'message')
		&& !(['refusal', 'reasoning', 'reasoning_content', 'annotations', 'tool_calls'].includes(key)
			&& (v == null || (Array.isArray(v) && v.length === 0)))));
	let content = displayContent(raw.content);
	if (Object.keys(extra).length) content += (content ? '\n\n' : '') + JSON.stringify(extra, null, 2);
	return { role, content, identity: canonical({ role, content: normalizedContent(raw.content), ...extra }) };
}
function item(value: unknown, fallback: string): ParsedMessage {
	if (!object(value)) return message(value, fallback);
	if (value.type === 'function_call') return message({ role: 'tool_call', ...value }, 'tool_call');
	if (value.type === 'function_call_output') return message({ role: 'tool', ...value }, 'tool');
	if (value.type === 'reasoning') return message({ role: 'reasoning', ...value }, 'reasoning');
	if (value.type === 'item_reference') return message({ role: 'reference', ...value }, 'reference');
	return message(value, fallback);
}
function inputMessages(value: unknown): ParsedMessage[] {
	if (!object(value)) return [message(value, 'request')];
	const result: ParsedMessage[] = [];
	for (const key of ['system', 'instructions']) if (value[key] != null) result.push(message(value[key], 'system'));
	if (Array.isArray(value.messages)) result.push(...value.messages.map(v => item(v, 'user')));
	if (has(value, 'input')) result.push(...(Array.isArray(value.input) ? value.input : [value.input]).map(v => item(v, 'user')));
	if (has(value, 'prompt')) result.push(message(value.prompt, 'user'));
	if (!result.length && !Array.isArray(value.messages) && !has(value, 'input')) result.push(message(value, 'request'));
	return result;
}
function outputMessages(value: unknown): ParsedOutput {
	if (!object(value)) {
		const m = message(value, 'response');
		return { messages: [m], branches: [], complete: false };
	}
	const complete = !value.error && !value.incomplete_details
		&& !['failed', 'incomplete', 'cancelled'].includes(String(value.status));
	if (Array.isArray(value.choices)) {
		const branches = value.choices.map((choice, index) => {
			if (!object(choice)) return [message(choice, 'response')];
			const m = message(choice.message ?? choice.delta ?? { content: choice.text ?? '', ...choice }, 'assistant');
			if ((value.choices as unknown[]).length > 1) m.label = `Choice ${typeof choice.index === 'number' ? choice.index + 1 : index + 1}`;
			return [m];
		});
		return { messages: branches.flat(), branches, complete };
	}
	if (Array.isArray(value.output)) {
		const messages = value.output.map(v => item(v, 'assistant'));
		return { messages: [...messages], branches: [messages], complete };
	}
	if (Array.isArray(value.content) || typeof value.content === 'string') {
		// Anthropic envelope metadata is not part of its echoed conversation message.
		const messages = [message({ role: value.role ?? 'assistant', content: value.content })];
		return { messages: [...messages], branches: [messages], complete };
	}
	if (typeof value.output_text === 'string') {
		const messages = [message(value.output_text, 'assistant')];
		return { messages: [...messages], branches: [messages], complete };
	}
	return { messages: [message(value, has(value, 'error') ? 'error' : 'response')], branches: [], complete: false };
}

/** SSE blocks are walked once; never split a whole large capture into a second array of strings. */
function* events(text: string): Generator<string> {
	let start = 0;
	const separator = /\r?\n\r?\n/g;
	for (let match; (match = separator.exec(text));) {
		yield text.slice(start, match.index);
		start = separator.lastIndex;
	}
	if (start < text.length) yield text.slice(start);
}
function set(target: Obj, key: string, value: unknown) {
	Object.defineProperty(target, key, { value, writable: true, enumerable: true, configurable: true });
}
function append(target: Obj, key: string, delta: unknown) {
	if (typeof delta === 'string') set(target, key, String(has(target, key) ? target[key] ?? '' : '') + delta);
}
function validIndex(value: unknown): boolean {
	return value === undefined || (typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value < 65536);
}
function safeDelta(value: unknown): boolean {
	if (Array.isArray(value)) return value.every(safeDelta);
	return !object(value) || (validIndex(value.index) && Object.values(value).every(safeDelta));
}
// OpenAI chat deltas, including reasoning, audio and all indexed tool calls.
function mergeDelta(target: Obj, delta: Obj) {
	for (const [key, value] of Object.entries(delta)) {
		if (key === 'index') continue;
		if (key === 'role') { set(target, key, value); continue; }
		if (typeof value === 'string') append(target, key, value);
		else if (Array.isArray(value)) {
			const parts = Array.isArray(target[key]) ? target[key] as unknown[] : [];
			set(target, key, parts);
			value.forEach((part, i) => {
				if (!object(part)) { parts.push(part); return; }
				const index = typeof part.index === 'number' ? part.index : i;
				if (!object(parts[index])) parts[index] = Object.create(null);
				mergeDelta(parts[index] as Obj, part);
			});
		} else if (object(value)) {
			if (!has(target, key) || !object(target[key])) set(target, key, Object.create(null));
			mergeDelta(target[key] as Obj, value);
		} else set(target, key, value);
	}
}
function streamMessages(text: string, notices: string[]): ParsedOutput {
	const chat = new Map<number, Obj>();
	const output = new Map<number, Obj>();
	const blocks = new Map<number, Obj>();
	const toolJson = new Map<number, string>();
	const extra: ParsedMessage[] = [];
	let final: unknown;
	let terminal = false;
	let complete = true;
	let anthropic = false;
	const outputItem = (index: number): Obj => {
		if (!output.has(index)) output.set(index, { type: 'message', role: 'assistant', content: [] });
		return output.get(index)!;
	};
	for (const block of events(text)) {
		const data = block.split(/\r?\n/).filter(line => line.startsWith('data:')).map(line => line.slice(5).replace(/^ /, '')).join('\n');
		if (!data) continue;
		if (data === '[DONE]') { terminal = true; continue; }
		let v: unknown;
		try { v = JSON.parse(data); } catch { extra.push(message(data, 'unparsed_event')); continue; }
		if (!object(v)) { extra.push(message(v, 'event')); continue; }
		if (![v.index, v.output_index, v.content_index, v.summary_index].every(validIndex) || !safeDelta(v.choices)) {
			extra.push(message(v, 'event')); continue;
		}
		if (Array.isArray(v.choices)) {
			for (const [i, choice] of v.choices.entries()) {
				if (!object(choice)) { extra.push(message(choice, 'event')); continue; }
				const index = typeof choice.index === 'number' ? choice.index : i;
				if (!chat.has(index)) chat.set(index, Object.create(null));
				if (object(choice.message)) chat.set(index, choice.message);
				else if (object(choice.delta)) mergeDelta(chat.get(index)!, choice.delta);
				else if (typeof choice.text === 'string') append(chat.get(index)!, 'content', choice.text);
			}
			continue;
		}
		const type = String(v.type ?? '');
		const index = typeof v.index === 'number' ? v.index : 0;
		const oi = typeof v.output_index === 'number' ? v.output_index : 0;
		const ci = typeof v.content_index === 'number' ? v.content_index : typeof v.summary_index === 'number' ? v.summary_index : 0;
		if (type === 'message_start') {
			anthropic = true;
			if (object(v.message)) array(v.message.content).forEach((part, i) => { if (object(part)) blocks.set(i, part); });
		} else if (type === 'message_stop') terminal = true;
		else if (type === 'content_block_start' && object(v.content_block)) { anthropic = true; blocks.set(index, v.content_block); }
		else if (type === 'content_block_delta' && object(v.delta)) {
			anthropic = true;
			if (!blocks.has(index)) blocks.set(index, Object.create(null));
			const part = blocks.get(index)!;
			const delta = v.delta;
			if (delta.type === 'text_delta') append(part, 'text', delta.text);
			else if (delta.type === 'thinking_delta') append(part, 'thinking', delta.thinking);
			else if (delta.type === 'signature_delta') append(part, 'signature', delta.signature);
			else if (delta.type === 'input_json_delta') toolJson.set(index, (toolJson.get(index) ?? '') + String(delta.partial_json ?? ''));
			else if (delta.type === 'citations_delta') part.citations = [...array(part.citations), delta.citation];
			else extra.push(message(v, 'event'));
		} else if (type === 'response.output_item.added' || type === 'response.output_item.done') {
			if (object(v.item)) output.set(oi, v.item); else extra.push(message(v, 'event'));
		} else if (type === 'response.reasoning_summary_part.added' || type === 'response.reasoning_summary_part.done') {
			const target = outputItem(oi); target.type = 'reasoning';
			const parts = array(target.summary); target.summary = parts; parts[ci] = v.part;
		} else if (type === 'response.content_part.added' || type === 'response.content_part.done') {
			const target = outputItem(oi);
			const parts = array(target.content); target.content = parts;
			parts[ci] = v.part;
		} else if (/^response\.(output_text|refusal|reasoning_text|reasoning_summary_text)\.(delta|done)$/.test(type)) {
			const target = outputItem(oi);
			if (type.includes('reasoning')) target.type = 'reasoning';
			const key = type.includes('summary') ? 'summary' : 'content';
			const parts = array(target[key]); set(target, key, parts);
			const field = type.includes('refusal') ? 'refusal' : 'text';
			if (!object(parts[ci])) parts[ci] = { type: type.includes('refusal') ? 'refusal' : type.includes('reasoning') ? 'summary_text' : 'output_text' };
			const part = parts[ci] as Obj;
			if (type.endsWith('.delta')) append(part, field, v.delta);
			else if (typeof v[field] === 'string') part[field] = v[field];
		} else if (type === 'response.function_call_arguments.delta' || type === 'response.function_call_arguments.done') {
			const target = outputItem(oi); target.type = 'function_call';
			delete target.content; delete target.role;
			if (typeof v.item_id === 'string') target.id = v.item_id;
			if (typeof v.name === 'string') target.name = v.name;
			if (typeof v.call_id === 'string') target.call_id = v.call_id;
			if (type.endsWith('.delta')) append(target, 'arguments', v.delta);
			else if (typeof v.arguments === 'string') target.arguments = v.arguments;
		} else if (['response.completed', 'response.failed', 'response.incomplete'].includes(type)) {
			terminal = true;
			complete &&= type === 'response.completed';
			if (object(v.response)) {
				if (array(v.response.output).length) final = v.response;
				if (v.response.error) extra.push(message(v.response.error, 'error'));
				if (v.response.incomplete_details) extra.push(message(v.response.incomplete_details, 'incomplete'));
			}
		} else if (!['ping', 'message_delta', 'content_block_stop', 'response.created', 'response.in_progress'].includes(type)) {
			extra.push(message(v, has(v, 'error') || type === 'error' ? 'error' : 'event'));
		}
	}
	if (!terminal) notices.push('The captured stream has no terminal event. All retained output is shown.');
	let parsed: ParsedOutput;
	if (final) parsed = outputMessages(final);
	else if (chat.size) parsed = outputMessages({ choices: [...chat].sort(([a], [b]) => a - b).map(([, m]) => ({ message: m })) });
	else if (anthropic) {
		for (const [index, text] of toolJson) {
			const part = blocks.get(index)!;
			try { part.input = JSON.parse(text); } catch { part.partial_json = text; complete = false; }
		}
		parsed = outputMessages({ content: [...blocks].sort(([a], [b]) => a - b).map(([, b]) => b) });
	} else parsed = outputMessages({ output: [...output].sort(([a], [b]) => a - b).map(([, v]) => v) });
	parsed.complete &&= complete && terminal && extra.length === 0;
	parsed.messages.push(...extra);
	return parsed;
}
function bodyText(body: SessionExportBody, direction: string, notices: string[]): string | null {
	if (body.status !== 'available' || body.data === null) { notices.push(`${direction} body is ${body.status}.`); return null; }
	if (body.truncated) notices.push(`${direction} body was truncated during capture. Only retained content is available.`);
	if (body.encoding === 'base64') {
		notices.push(`${direction} body contains binary data; its complete captured base64 is shown.`);
		return null;
	}
	return body.data;
}
export function parseRequest(record: ExportRequest): ParsedRequest {
	const notices: string[] = [];
	const input: ParsedMessage[] = [];
	let tools: ParsedRequest['tools'] = null;
	let context: ParsedRequest['context'] = 'snapshot';
	let inputComplete = false;
	let result: ParsedOutput = { messages: [], branches: [], complete: false };
	const request = bodyText(record.request_body, 'Request', notices);
	const response = bodyText(record.response_body, 'Response', notices);
	if (request) {
		try {
			const parsed = JSON.parse(request);
			input.push(...inputMessages(parsed));
			tools = toolDeclarations(parsed);
			inputComplete = !record.request_body.truncated;
			if (object(parsed) && (parsed.previous_response_id != null || parsed.conversation != null
				|| (Array.isArray(parsed.input) ? parsed.input : [parsed.input]).some(item => object(item) && item.type === 'item_reference'))) {
				context = 'provider-managed';
				notices.push('This request uses provider-managed history. Its input is shown without folding repeated context.');
			}
		}
		catch { input.push(message(request, 'request')); notices.push('Request body is not valid JSON; the captured text is shown unchanged.'); }
	}
	if (response) {
		try {
			const parsed = JSON.parse(response); result = outputMessages(parsed);
			if (object(parsed) && (Array.isArray(parsed.output) || Array.isArray(parsed.content))) {
				if (parsed.error) result.messages.push(message(parsed.error, 'error'));
				if (parsed.incomplete_details) result.messages.push(message(parsed.incomplete_details, 'incomplete'));
			}
		}
		catch {
			if (/(^|\n)data:/.test(response)) result = streamMessages(response, notices);
			else { result.messages.push(message(response, 'response')); notices.push('Response body is not recognized; the captured text is shown unchanged.'); }
		}
	}
	for (const [body, target, role] of [[record.request_body, input, 'request_base64'], [record.response_body, result.messages, 'response_base64']] as const) {
		if (body.status === 'available' && body.encoding === 'base64' && body.data !== null) target.push(message(body.data, role));
	}
	const interrupted = record.request.tags.includes('capture_interrupted');
	if (interrupted) notices.push('Capture was interrupted.');
	return {
		input, output: result.messages, outputBranches: result.branches, notices, tools, context, inputComplete,
		outputComplete: result.complete && !record.response_body.truncated && !interrupted
	};
}
