export interface ToolDeclaration {
	source: 'tools' | 'functions';
	value: unknown;
}
export interface ToolDefinition {
	name: string;
	description: string | null;
	kind: string | null;
	/** Full definition, including schema and provider-specific options; formatted off-thread for transcripts. */
	definition: string;
}
export interface ToolSet {
	id: number;
	tools: ToolDefinition[];
}
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);

/** Read only definitions actually supplied in this request, including legacy function declarations. */
export function toolDeclarations(request: unknown): ToolDeclaration[] {
	if (!object(request)) return [];
	const declarations: ToolDeclaration[] = [];
	for (const source of ['tools', 'functions'] as const) {
		const value = request[source];
		if (value == null) continue;
		for (const item of Array.isArray(value) ? value : [value]) declarations.push({ source, value: item });
	}
	return declarations;
}
export function describeTools(declarations: ToolDeclaration[]): ToolDefinition[] {
	return declarations.map(({ source, value }, index) => {
		const tool = object(value) ? value : {};
		const definition = object(tool.function) ? tool.function : tool;
		const name = typeof definition.name === 'string' && definition.name ? definition.name
			: typeof tool.type === 'string' && tool.type ? tool.type : `Unnamed tool ${index + 1}`;
		return {
			name,
			description: typeof definition.description === 'string' ? definition.description : null,
			kind: typeof tool.type === 'string' ? tool.type
				: source === 'functions' || object(tool.function) || Object.hasOwn(tool, 'input_schema') ? 'function' : null,
			definition: JSON.stringify(value, null, 2)
		};
	});
}
