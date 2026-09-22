import type { QuerySchema } from '$lib/api/types';
import { queryField, queryOperators, queryTokens } from './query-text';

export interface QuerySuggestion {
	label: string;
	type: string;
	detail?: string;
	apply?: string;
	info?: string;
}

const keyword = (label: string): QuerySuggestion => ({ label, type: 'keyword' });
const quotedName = (name: string) => /^[a-z_][a-z0-9_]*$/.test(name) ? name : `"${name.replaceAll('"', '""')}"`;

/** Return only suggestions supported by the server schema and the editor grammar. */
export function querySuggestions(text: string, position: number, schema: QuerySchema): { from: number; options: QuerySuggestion[] } {
	const before = text.slice(0, position);
	const current = queryTokens(before, true, true).at(-1);
	if (current && ['string', 'identifier', 'comment'].includes(current.kind) && current.to === position
		&& !(current.kind === 'comment' && current.text.endsWith('\n'))) return { from: position, options: [] };
	const match = /[a-zA-Z_][a-zA-Z0-9_.-]*$/.exec(before);
	const from = match ? position - match[0].length : position;
	const tokens = queryTokens(before.slice(0, from), true);
	const full = queryTokens(text, true);
	const fromIndex = full.findIndex((t) => t.kind === 'word' && t.text.toUpperCase() === 'FROM');
	const datasetName = full[fromIndex + 1];
	const dataset = fromIndex >= 0 ? schema.datasets.find((d) => d.name === (datasetName?.kind === 'word' ? datasetName.text.toLowerCase() : datasetName?.text)) : undefined;
	const words = tokens.map((t) => t.kind === 'word' ? t.text.toUpperCase() : t.text);
	const last = words.at(-1);
	const names = () => {
		const fields = dataset?.fields ?? schema.datasets.flatMap((d) => d.fields);
		const options: QuerySuggestion[] = [...new Map(fields.map((f) => [f.name, f])).values()].map((f) => ({
			label: f.name, apply: quotedName(f.name), type: 'property', detail: f.filter_kind ?? 'field',
			info: `${dataset?.name ?? 'Dataset'} field${f.operators.length ? ` · ${f.operators.map((op) => queryOperators[op]).join(', ')}` : ''}`
		}));
		if (dataset?.name === schema.plugin_metadata.dataset) options.push({
			label: 'plugin_metadata.', type: 'property', detail: 'plugin field path',
			apply: 'plugin_metadata.', info: 'Continue with plugin name and field path, e.g. plugin_metadata.identity.team.'
		});
		return options;
	};
	let options: QuerySuggestion[];
	if (!tokens.length) options = [keyword('SELECT')];
	else if (last === 'FROM') options = schema.datasets.map((d) => ({ label: d.name, type: 'class', detail: 'dataset', apply: quotedName(d.name), info: `${d.fields.length} queryable fields` }));
	else if (last === 'ORDER') options = [keyword('BY')];
	else if (last === 'IS') options = [keyword('NULL'), keyword('NOT NULL')];
	else if (last === 'NOT' && words.at(-2) === 'IS') options = [keyword('NULL')];
	else if (last === 'LIMIT') options = [25, 100, schema.limits.max_limit].filter((n, i, all) => all.indexOf(n) === i).map((n) => ({ label: String(n), type: 'constant', detail: 'rows' }));
	else {
		let clause = '';
		for (const token of tokens) if (token.kind === 'word' && ['SELECT', 'FROM', 'WHERE', 'ORDER', 'LIMIT'].includes(token.text.toUpperCase())) clause = token.text.toUpperCase();
		const previous = tokens.at(-1);
		const fieldName = previous?.kind === 'word' ? previous.text.toLowerCase() : previous?.text;
		const field = dataset && fieldName ? queryField(schema, dataset, fieldName) : undefined;
		if (clause === 'SELECT') options = [...names(), keyword('FROM'), { label: '*', type: 'keyword', detail: 'all dataset fields' }];
		else if (clause === 'FROM') options = ['WHERE', 'ORDER BY', 'LIMIT'].map(keyword);
		else if (clause === 'ORDER') options = last === 'BY' || last === ',' ? names() : ['ASC', 'DESC', 'LIMIT'].map(keyword);
		else if (clause === 'WHERE') {
			if (last === 'WHERE' || last === 'AND') options = names().filter((o) => o.label.startsWith(schema.plugin_metadata.field_prefix) || (dataset?.fields.find((f) => f.name === o.label)?.operators.length ?? 0) > 0);
			else if (field) options = field.operators.map((op) => keyword(queryOperators[op]));
			else if (['=', '!=', '<>', '>', '>=', '<', '<=', 'CONTAINS'].includes(last ?? '')) {
				const name = tokens.at(-2)?.text;
				const kind = name && dataset ? queryField(schema, dataset, name)?.filter_kind : null;
				if (kind === 'bool') options = ['TRUE', 'FALSE'].map(keyword);
				else if (kind === 'int') options = [0, 200, 400, 500].map((n) => ({ label: String(n), type: 'constant' }));
				else if (kind === 'json' || kind === 'json_path') options = [keyword('TRUE'), keyword('FALSE'), { label: "JSON '{}'", type: 'keyword', detail: 'JSON value' }];
				else options = [];
			} else options = ['AND', 'ORDER BY', 'LIMIT'].map(keyword);
		} else options = [];
	}
	return { from, options };
}
