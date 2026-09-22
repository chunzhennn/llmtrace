<script lang="ts">
	import { onMount } from 'svelte';
	import { EditorState, Prec } from '@codemirror/state';
	import { EditorView, keymap, lineNumbers, highlightActiveLine, drawSelection } from '@codemirror/view';
	import { defaultKeymap, history, historyKeymap, insertNewlineAndIndent } from '@codemirror/commands';
	import { autocompletion, completionKeymap, acceptCompletion, completionStatus, type CompletionContext } from '@codemirror/autocomplete';
	import { sql, SQLDialect } from '@codemirror/lang-sql';
	import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
	import { tags } from '@lezer/highlight';
	import { linter, type Diagnostic } from '@codemirror/lint';
	import type { QuerySchema } from '$lib/api/types';
	import { parseQueryText, QueryTextError } from '$lib/utils/query-text';
	import { querySuggestions } from '$lib/utils/query-completion';

	interface Props {
		value: string;
		schema: QuerySchema;
		onRun: () => void;
	}
	let { value = $bindable(), schema, onRun }: Props = $props();
	let container: HTMLDivElement;
	let editor: EditorView | undefined;

	onMount(() => {
		const completion = (context: CompletionContext) => {
			const result = querySuggestions(context.state.doc.toString(), context.pos, schema);
			return result.options.length ? result : null;
		};
		const theme = EditorView.theme({
			'&': { backgroundColor: 'var(--color-canvas)', color: 'var(--color-fg)', fontSize: '13px' },
			'.cm-content': { minHeight: '220px', padding: '12px 0', caretColor: 'var(--color-fg)', fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' },
			'.cm-line': { padding: '0 12px' },
			'.cm-scroller': { overflow: 'auto', maxHeight: '32rem' },
			'.cm-gutters': { backgroundColor: 'var(--color-surface)', color: 'var(--color-fg-muted)', borderRight: '1px solid var(--color-border)' },
			'.cm-activeLine, .cm-activeLineGutter': { backgroundColor: 'color-mix(in srgb, var(--color-brand) 6%, transparent)' },
			'&.cm-focused': { outline: 'none' },
			'.cm-cursor': { borderLeftColor: 'var(--color-fg)' },
			'&.cm-focused .cm-selectionBackground, .cm-selectionBackground': { backgroundColor: 'color-mix(in srgb, var(--color-brand) 22%, transparent)' },
			'.cm-tooltip': { backgroundColor: 'var(--color-surface)', color: 'var(--color-fg)', border: '1px solid var(--color-border)', borderRadius: '6px' },
			'.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: 'var(--color-brand)', color: 'var(--color-brand-fg)' },
			'.cm-completionDetail': { marginLeft: '1rem', opacity: '0.8' },
			'.cm-diagnostic': { fontFamily: 'inherit' }
		});
		editor = new EditorView({
			parent: container,
			state: EditorState.create({
				doc: value,
				extensions: [
					lineNumbers(), history(), drawSelection(), highlightActiveLine(), EditorView.lineWrapping,
					sql({ dialect: SQLDialect.define({ keywords: 'select from where and order by limit asc desc contains is not null true false json', caseInsensitiveIdentifiers: true }) }),
					syntaxHighlighting(HighlightStyle.define([
						{ tag: tags.keyword, color: 'var(--color-brand)' },
						{ tag: tags.string, color: 'var(--color-success)' },
						{ tag: [tags.number, tags.bool, tags.null], color: 'var(--color-warning)' },
						{ tag: tags.comment, color: 'var(--color-fg-muted)', fontStyle: 'italic' },
						{ tag: tags.operator, color: 'var(--color-info)' }
					])), theme,
					EditorView.contentAttributes.of({ 'aria-label': 'Query statement', 'aria-describedby': 'query-editor-help', spellcheck: 'false' }),
					autocompletion({ override: [completion] }),
					Prec.highest(keymap.of([
						{ key: 'Enter', run: (view) => {
							if (view.composing) return false;
							if (completionStatus(view.state)) return acceptCompletion(view);
							onRun();
							return true;
						} },
						{ key: 'Shift-Enter', run: insertNewlineAndIndent },
						{ key: 'Mod-Enter', run: () => { onRun(); return true; } },
						{ key: 'Tab', run: acceptCompletion }
					])),
					keymap.of([...completionKeymap, ...defaultKeymap, ...historyKeymap]),
					linter((view): Diagnostic[] => {
						try { parseQueryText(view.state.doc.toString(), schema); return []; }
						catch (error) {
							if (!(error instanceof QueryTextError)) return [];
							return [{ from: Math.min(error.from, view.state.doc.length), to: Math.min(error.to, view.state.doc.length), severity: 'error', message: error.message }];
						}
					}, { delay: 350 }),
					EditorView.updateListener.of((update) => { if (update.docChanged) value = update.state.doc.toString(); })
				]
			})
		});
		return () => { editor?.destroy(); editor = undefined; };
	});

	$effect(() => {
		const text = value;
		if (editor && editor.state.doc.toString() !== text) {
			editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: text } });
		}
	});
</script>

<div class="min-w-0 overflow-hidden rounded-lg border border-[var(--color-border)] focus-within:border-[var(--color-brand)]" bind:this={container}></div>
