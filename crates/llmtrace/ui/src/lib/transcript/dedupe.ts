import type { ContextNode, ExportRequest, ParsedMessage, ParsedRequest, TranscriptBlock, TranscriptMessage } from './types';
import { canonical } from './parse';
import { describeTools, type ToolSet } from './tools';

interface Node extends ContextNode { children: Map<string, Node> }

/** A prefix trie, not a global set of message texts. A branch is never spliced into another path. */
export class TranscriptHistory {
	private root: Node = { id: 0, parent: 0, messageId: '', children: new Map() };
	private nextNode = 1;
	private toolSets = new Map<string, ToolSet>();
	private previousToolSet: number | null = null;

	add(record: ExportRequest, parsed: ParsedRequest): { block: TranscriptBlock; nodes: ContextNode[]; toolSet?: ToolSet } {
		let toolSet: ToolSet | undefined;
		let toolSetId: number | null = null;
		let toolsChanged = false;
		if (parsed.tools !== null) {
			const key = canonical(parsed.tools);
			let known = this.toolSets.get(key);
			if (!known) {
				known = { id: this.toolSets.size + 1, tools: describeTools(parsed.tools) };
				this.toolSets.set(key, known);
				toolSet = known;
			}
			toolSetId = known.id;
			toolsChanged = toolSetId !== this.previousToolSet && (known.tools.length > 0 || this.previousToolSet !== null);
		}
		this.previousToolSet = toolSetId;
		const nodes: ContextNode[] = [];
		const messages: TranscriptMessage[] = [];
		let ordinal = 0;
		const emit = (message: ParsedMessage) => {
			const shown = { id: `${record.request.id}:${ordinal++}`, role: message.role, label: message.label, content: message.content };
			messages.push(shown);
			return shown.id;
		};
		const edge = (parent: Node, message: ParsedMessage, messageId: string): Node => {
			let node = parent.children.get(message.identity);
			if (!node) {
				node = { id: this.nextNode++, parent: parent.id, messageId, children: new Map() };
				parent.children.set(message.identity, node);
				nodes.push({ id: node.id, parent: node.parent, messageId });
			}
			return node;
		};
		let current = this.root;
		let reusedCount = 0;
		let reusedNode = 0;
		const remember = parsed.inputComplete && parsed.context === 'snapshot';
		let matching = remember;
		for (const message of parsed.input) {
			const known = matching ? current.children.get(message.identity) : undefined;
			if (known) {
				current = known;
				reusedCount++;
				reusedNode = known.id;
			} else {
				matching = false;
				const id = emit(message);
				if (remember) current = edge(current, message, id);
			}
		}
		// Outputs are new generation attempts, even if their text happens to repeat.
		const outputIds = new Map<ParsedMessage, string>();
		for (const message of parsed.output) outputIds.set(message, emit(message));
		if (remember && parsed.outputComplete) {
			for (const branch of parsed.outputBranches) {
				let tip = current;
				for (const message of branch) tip = edge(tip, message, outputIds.get(message)!);
			}
		}
		return { block: { id: record.request.id, createdAt: record.request.started_at, toolSetId, toolsChanged, messages, reusedCount, reusedNode, notices: parsed.notices }, nodes, toolSet };
	}
}
