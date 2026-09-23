// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
import { EditorView } from '@codemirror/view';
import Page from './+page.svelte';
import * as queryApi from '$lib/api/endpoints/query';
import type { QuerySchema } from '$lib/api/types';
import { parseQueryText } from '$lib/utils/query-text';

vi.mock('$lib/api/endpoints/query', () => ({ querySchema: vi.fn(), runQuery: vi.fn() }));

const schema: QuerySchema = {
	limits: { max_fields: 3, max_filters: 3, max_order_by: 2, max_limit: 500, max_string_value_bytes: 4096, max_json_value_bytes: 16384 },
	sort_directions: ['asc', 'desc'],
	plugin_metadata: { dataset: 'requests', field_prefix: 'plugin_metadata.', filter_kind: 'json_path', operators: ['eq', 'is_null'], max_segments: 16, max_segment_bytes: 64, segment_pattern: '[A-Za-z0-9_-]+' },
	datasets: [{ name: 'requests', default_fields: ['id'], default_order: [{ field: 'id', direction: 'desc' }], fields: [
		{ name: 'id', filter_kind: 'uuid', operators: ['eq', 'is_null'] },
		{ name: 'plugin_metadata', filter_kind: 'json', operators: ['eq', 'is_null'] },
		{ name: 'status', filter_kind: 'int', operators: ['eq', 'gte', 'is_null'] },
		{ name: 'model', filter_kind: 'text', operators: ['eq', 'contains', 'is_null'] },
		{ name: 'usage_complete', filter_kind: 'bool', operators: ['eq', 'is_null'] }
	] }, { name: 'sessions', default_fields: ['session_key'], default_order: [{ field: 'session_key', direction: 'asc' }], fields: [
		{ name: 'session_key', filter_kind: 'text', operators: ['eq'] }
	] }]
};
let target: HTMLDivElement;
let component: ReturnType<typeof mount>;

beforeEach(async () => {
	vi.mocked(queryApi.querySchema).mockResolvedValue(schema);
	vi.mocked(queryApi.runQuery).mockResolvedValue({ dataset: 'requests', fields: ['id'], rows: [], limit: 100 });
	target = document.createElement('div');
	document.body.append(target);
	component = mount(Page, { target });
	await vi.waitFor(() => expect(target.querySelector('[role="tab"]')).not.toBeNull());
});
afterEach(async () => {
	await unmount(component);
	target.remove();
	vi.clearAllMocks();
});

async function click(label: string) {
	const button = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === label);
	expect(button, `Button ${label}`).toBeDefined();
	button!.click();
	await tick();
}
function editor() {
	return EditorView.findFromDOM(target.querySelector<HTMLElement>('.cm-editor')!)!;
}
async function writeCode(text: string) {
	const view = editor();
	view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
	await tick();
}

async function setControl(selector: string, value: string) {
	const control = target.querySelector<HTMLInputElement | HTMLSelectElement>(selector)!;
	expect(control).not.toBeNull();
	control.value = value;
	control.dispatchEvent(new Event(control.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }));
	await tick();
}

function column(name: string) {
	const label = [...target.querySelectorAll('.columns-panel label')].find(label => label.textContent?.trim() === name);
	return label?.querySelector<HTMLInputElement>('input');
}

it('adds the chosen field and sends typed AND conditions with readable operators', async () => {
	await setControl('[aria-label="Add filter"]', 'status');
	await setControl('[aria-label="Filter operator"]', 'gte');
	await setControl('[aria-label="Filter value"]', '400');
	expect(target.querySelector<HTMLOptionElement>('[aria-label="Filter operator"] option:checked')?.textContent).toBe('at least');
	await setControl('[aria-label="Add filter"]', 'usage_complete');
	expect(target.querySelector<HTMLSelectElement>('select[aria-label="Filter value"]')?.value).toBe('true');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: [
		{ field: 'status', op: 'gte', value: 400 },
		{ field: 'usage_complete', op: 'eq', value: true }
	] }));
});

it('resets incompatible values and operators when changing field types', async () => {
	await setControl('[aria-label="Add filter"]', 'status');
	await setControl('[aria-label="Filter operator"]', 'gte');
	await setControl('[aria-label="Filter value"]', '400');
	await setControl('[aria-label="Filter field"]', 'usage_complete');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: [
		{ field: 'usage_complete', op: 'eq', value: true }
	] }));
	await setControl('[aria-label="Filter field"]', 'model');
	expect(target.querySelector<HTMLInputElement>('[aria-label="Filter value"]')?.value).toBe('');
});

it('omits the value for missing-field filters and supports removing conditions', async () => {
	await setControl('[aria-label="Add filter"]', 'model');
	await setControl('[aria-label="Filter value"]', 'gpt');
	await setControl('[aria-label="Filter operator"]', 'is_null');
	expect(target.querySelector('[aria-label="Filter value"]')).toBeNull();
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: [{ field: 'model', op: 'is_null' }] }));
	target.querySelector<HTMLButtonElement>('[aria-label="Remove filter"]')!.click();
	await tick();
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: undefined }));
});

it('adds plugin metadata paths through the same field picker', async () => {
	await setControl('[aria-label="Add filter"]', '__plugin__');
	await setControl('[aria-label="Plugin metadata path"]', 'plugin_metadata.identity.team');
	await setControl('[aria-label="Filter value"]', '{"name":"research"}');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: [
		{ field: 'plugin_metadata.identity.team', op: 'eq', value: { name: 'research' } }
	] }));
});

it('initializes a boolean value when converting a valueless Code condition to equality', async () => {
	await click('Code');
	await writeCode('SELECT id FROM requests WHERE usage_complete IS NULL');
	await click('Builder');
	await setControl('[aria-label="Filter operator"]', 'eq');
	expect(target.querySelector<HTMLSelectElement>('[aria-label="Filter value"]')?.value).toBe('true');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ filters: [
		{ field: 'usage_complete', op: 'eq', value: true }
	] }));
});

it('searches columns without losing selections and respects minimum and maximum counts', async () => {
	expect(column('id')?.disabled).toBe(true);
	await setControl('[aria-label="Search columns"]', 'MODEL');
	column('model')!.click();
	await tick();
	await setControl('[aria-label="Search columns"]', 'status');
	column('status')!.click();
	await tick();
	await setControl('[aria-label="Search columns"]', '');
	expect(column('usage_complete')?.disabled).toBe(true);
	expect(column('id')?.disabled).toBe(false);
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ fields: ['id', 'model', 'status'] }));
	await click('Use defaults');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({ fields: ['id'] }));
});

it('caps filters and sort keys and resets controls for another dataset', async () => {
	for (const field of ['id', 'model', 'status']) await setControl('[aria-label="Add filter"]', field);
	expect(target.querySelector<HTMLSelectElement>('[aria-label="Add filter"]')?.disabled).toBe(true);
	await click('Add sort');
	expect([...target.querySelectorAll('button')].find(button => button.textContent?.trim() === 'Add sort')?.disabled).toBe(true);
	await setControl('[aria-label="Dataset"]', 'sessions');
	expect(target.querySelector('[aria-label="Filter field"]')).toBeNull();
	expect(target.querySelector('[aria-label="Add filter"] option[value="__plugin__"]')).toBeNull();
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenLastCalledWith(expect.objectContaining({
		dataset: 'sessions', fields: ['session_key'], filters: undefined,
		order_by: [{ field: 'session_key', direction: 'asc' }]
	}));
});

it.each(['0', '501', '1.5', ''])('blocks invalid row limit %j before sending the query', async (limit) => {
	await setControl('#query-limit', limit);
	await click('Run query');
	expect(queryApi.runQuery).not.toHaveBeenCalled();
	expect(target.querySelector('[role="alert"]')?.textContent).toContain('between 1 and 500');
});

it('executes the same query after Code to Builder to Code conversion', async () => {
	await click('Code');
	const code = `SELECT id, "plugin_metadata.My-plugin.team" FROM requests
		WHERE plugin_metadata = JSON '{"label":"123","n":null}'
		ORDER BY "plugin_metadata.My-plugin.team" ASC LIMIT 7;`;
	await writeCode(code);
	await click('Builder');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenCalledWith(parseQueryText(code, schema));
	await click('Code');
	expect(parseQueryText(editor().state.doc.toString(), schema)).toEqual(parseQueryText(code, schema));
});

it('retains invalid drafts when switching modes and after editing Builder settings', async () => {
	await click('Code');
	const draft = 'SELECT id FROM requests WHERE';
	await writeCode(draft);
	await click('Builder');
	await click('Code');
	expect(editor().state.doc.toString()).toBe(draft);
	await click('Builder');
	const limit = target.querySelector<HTMLInputElement>('input[type="number"]')!;
	limit.value = '25';
	limit.dispatchEvent(new Event('input', { bubbles: true }));
	await tick();
	await click('Code');
	expect(parseQueryText(editor().state.doc.toString(), schema).limit).toBe(25);
	await click('Restore code draft');
	expect(editor().state.doc.toString()).toBe(draft);
});

it('blocks standalone JSON null before calling the query API and accepts IS NULL', async () => {
	await click('Code');
	await writeCode("SELECT id FROM requests WHERE plugin_metadata = JSON 'null'");
	await click('Run query');
	expect(queryApi.runQuery).not.toHaveBeenCalled();
	expect(target.querySelector('[role="alert"]')?.textContent).toContain('Use IS NULL or IS NOT NULL');
	await writeCode('SELECT id FROM requests WHERE plugin_metadata IS NULL');
	await click('Run query');
	expect(queryApi.runQuery).toHaveBeenCalledOnce();
});
