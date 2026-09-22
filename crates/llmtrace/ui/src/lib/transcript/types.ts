import type { SessionExportRecord } from '$lib/api/types';
import type { ToolDeclaration, ToolSet } from './tools';

export type ExportHeader = Extract<SessionExportRecord, { type: 'session' }>;
export type ExportRequest = Extract<SessionExportRecord, { type: 'request' }>;
export type ExportEnd = Extract<SessionExportRecord, { type: 'end' }>;

export interface ParsedMessage {
	role: string;
	label?: string;
	content: string;
	/** Exact normalized message, including tool IDs and non-text content. Never match on display text. */
	identity: string;
}
export interface ParsedRequest {
	/** Null when the request body could not be decoded; never inherit another request's tools. */
	tools: ToolDeclaration[] | null;
	input: ParsedMessage[];
	output: ParsedMessage[];
	/** Provider-managed history is not a full snapshot and cannot establish a reusable prefix. */
	context: 'snapshot' | 'provider-managed';
	inputComplete: boolean;
	outputComplete: boolean;
	notices: string[];
	/** Multiple choices are alternatives, not one conversation path. */
	outputBranches: ParsedMessage[][];
}
export interface TranscriptMessage {
	id: string;
	role: string;
	label?: string;
	content: string;
}
export interface ContextNode {
	id: number;
	parent: number;
	messageId: string;
}
export interface TranscriptBlock {
	toolSetId: number | null;
	toolsChanged: boolean;
	id: string;
	createdAt: string;
	messages: TranscriptMessage[];
	reusedCount: number;
	reusedNode: number;
	notices: string[];
}
export type TranscriptEvent =
	| { type: 'header'; header: ExportHeader }
	| { type: 'request'; block: TranscriptBlock; nodes: ContextNode[]; toolSet?: ToolSet }
	| { type: 'end'; end: ExportEnd }
	| { type: 'error'; message: string; status?: number };
