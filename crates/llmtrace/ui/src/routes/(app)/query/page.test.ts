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
	limits: { max_fields: 64, max_filters: 32, max_order_by: 8, max_limit: 500, max_string_value_bytes: 4096, max_json_value_bytes: 16384 },
	sort_directions: ['asc', 'desc'],
	plugin_metadata: { dataset: 'requests', field_prefix: 'plugin_metadata.', filter_kind: 'json_path', operators: ['eq', 'is_null'], max_segments: 16, max_segment_bytes: 64, segment_pattern: '[A-Za-z0-9_-]+' },
	datasets: [{ name: 'requests', default_fields: ['id'], default_order: [{ field: 'id', direction: 'desc' }], fields: [
		{ name: 'id', filter_kind: 'uuid', operators: ['eq', 'is_null'] },
		{ name: 'plugin_metadata', filter_kind: 'json', operators: ['eq', 'is_null'] }
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
