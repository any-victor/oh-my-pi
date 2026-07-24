import { prompt } from "@oh-my-pi/pi-utils";
import autoHandoffFocus from "./prompts/auto-handoff-threshold-focus.md" with { type: "text" };
import branchSummary from "./prompts/branch-summary.md" with { type: "text" };
import branchSummaryContext from "./prompts/branch-summary-context.md" with { type: "text" };
import branchSummaryPreamble from "./prompts/branch-summary-preamble.md" with { type: "text" };
import compactionShortSummary from "./prompts/compaction-short-summary.md" with { type: "text" };
import compactionSummary from "./prompts/compaction-summary.md" with { type: "text" };
import compactionSummaryContext from "./prompts/compaction-summary-context.md" with { type: "text" };
import compactionTurnPrefix from "./prompts/compaction-turn-prefix.md" with { type: "text" };
import compactionUpdateSummary from "./prompts/compaction-update-summary.md" with { type: "text" };
import fileOperations from "./prompts/file-operations.md" with { type: "text" };
import handoffContext from "./prompts/handoff-context.md" with { type: "text" };
import handoffDocument from "./prompts/handoff-document.md" with { type: "text" };
import snapcompactArchiveContext from "./prompts/snapcompact-archive-context.md" with { type: "text" };
import summarizationSystem from "./prompts/summarization-system.md" with { type: "text" };

/**
 * Template data accepted by each compaction/handoff prompt — the single key
 * registry for the template system and the compile-checked mirror of the
 * "Template data" table in `docs/compaction.md`. Pairing a key with another
 * key's context is a type error.
 */
export interface CompactionPromptContexts {
	summarizationSystem: Record<string, never>;
	summary: SummaryPromptContext;
	updateSummary: SummaryPromptContext;
	shortSummary: {
		conversation: string;
		previousSummary?: string;
		additionalContext?: string[];
	};
	turnPrefix: { conversation: string };
	handoffDocument: { additionalFocus?: string; additionalContext?: string[] };
	handoffContext: { document: string };
	autoHandoffFocus: Record<string, never>;
	snapcompactArchiveContext: { archiveText: string };
	branchSummary: { conversation: string; customInstructions?: string };
	branchSummaryPreamble: Record<string, never>;
	compactionSummaryContext: { summary: string };
	branchSummaryContext: { summary: string };
	fileOperations: { files: string };
}

/** Configured overrides, keyed by the same registry: `COMPACTION.yml` shape. */
export type CompactionPromptTemplates = { [K in keyof CompactionPromptContexts]?: string };

interface SummaryPromptContext {
	conversation: string;
	previousSummary?: string;
	additionalContext?: string[];
	additionalFocus?: string;
	promptOverride?: string;
}

/**
 * A resolved compaction/handoff template set: configured overrides layered
 * over the bundled defaults. Constructed once per operation, so a set is also
 * the natural snapshot unit when configuration is re-read between operations.
 *
 * Rendering is verbatim Handlebars: helpers see the real field values
 * (`{{len conversation}}`, `{{jsonStringify conversation}}`, `{{#if}}`,
 * `{{#each}}` all behave), and the prose formatter never runs, so
 * conversation text and caller-supplied context reach the provider
 * byte-for-byte — matching the pre-template behavior where raw text was
 * concatenated around a static prompt.
 */
export class CompactionPrompts {
	readonly #overrides: CompactionPromptTemplates;

	constructor(overrides?: CompactionPromptTemplates) {
		// Clone so an instance is a true snapshot: later mutation of the caller's
		// object cannot change an in-flight operation.
		this.#overrides = { ...overrides };
	}

	/** The template text used for `key`: configured override or bundled default. */
	get(key: keyof CompactionPromptTemplates): string {
		return this.#overrides[key] ?? DEFAULT_COMPACTION_PROMPT_TEMPLATES[key];
	}

	render<K extends keyof CompactionPromptTemplates>(key: K, context: CompactionPromptContexts[K]): string {
		const template = this.get(key);
		// The per-key context types are exact; the underlying renderer takes the
		// loose Handlebars TemplateContext shape.
		const compiled = prompt.compile(template);
		return compiled(context as prompt.TemplateContext);
	}
}

export const DEFAULT_COMPACTION_PROMPT_TEMPLATES: Required<CompactionPromptTemplates> = {
	summarizationSystem: summarizationSystem.endsWith("\n") ? summarizationSystem.slice(0, -1) : summarizationSystem,
	summary: compactionSummary,
	updateSummary: compactionUpdateSummary,
	shortSummary: compactionShortSummary,
	turnPrefix: compactionTurnPrefix,
	handoffDocument,
	handoffContext,
	autoHandoffFocus,
	snapcompactArchiveContext,
	branchSummary,
	branchSummaryPreamble,
	compactionSummaryContext: compactionSummaryContext.endsWith("\n")
		? compactionSummaryContext.slice(0, -1)
		: compactionSummaryContext,
	branchSummaryContext: branchSummaryContext.endsWith("\n") ? branchSummaryContext.slice(0, -1) : branchSummaryContext,
	fileOperations,
};
