import * as fs from "node:fs/promises";
import type { CompactionPromptTemplates } from "@oh-my-pi/pi-agent-core/compaction";
import { logger, prompt } from "@oh-my-pi/pi-utils";
import { type } from "arktype";
import { YAML } from "bun";
import { collectConfigCandidates } from "../advisor/watchdog";
import { expandAtImports } from "../discovery/at-imports";

const compactionPromptTemplatesSchema = type({
	"summarizationSystem?": "string",
	"summary?": "string",
	"updateSummary?": "string",
	"shortSummary?": "string",
	"turnPrefix?": "string",
	"handoffDocument?": "string",
	"handoffContext?": "string",
	"autoHandoffFocus?": "string",
	"snapcompactArchiveContext?": "string",
	"branchSummary?": "string",
	"branchSummaryPreamble?": "string",
	"compactionSummaryContext?": "string",
	"branchSummaryContext?": "string",
	"fileOperations?": "string",
});

const compactionConfigShape = type({
	"prompts?": compactionPromptTemplatesSchema,
});

const compactionConfigSchema = compactionConfigShape.onDeepUndeclaredKey("reject");

/** Configured prompt overrides discovered from `COMPACTION.yml`/`.yaml`. */
export interface DiscoveredCompactionConfig {
	prompts: CompactionPromptTemplates;
}

/**
 * Render validation twice so both truthy and inverse branches of optional
 * template guards execute before a config is accepted.
 */
const TEMPLATE_VALIDATION_CONTEXTS = [
	{
		conversation: "sample",
		previousSummary: "sample",
		additionalContext: ["sample"],
		additionalFocus: "sample",
		promptOverride: "sample",
		customInstructions: "sample",
		document: "sample",
		archiveText: "sample",
		summary: "sample",
		files: "sample",
	},
	{},
] as const;

/**
 * Discover prompt overrides from `COMPACTION.yml`/`COMPACTION.yaml` on the
 * shared user and project config search path. Files are merged user-first then
 * ancestor-to-leaf, so every supplied prompt field independently takes the
 * most-specific value. Invalid files are logged and skipped in full.
 */
export async function discoverCompactionConfig(cwd: string, agentDir?: string): Promise<DiscoveredCompactionConfig> {
	const candidates = await collectConfigCandidates(cwd, agentDir, ["COMPACTION.yml", "COMPACTION.yaml"]);
	const prompts: CompactionPromptTemplates = {};

	for (const candidate of candidates) {
		let parsed: unknown;
		try {
			parsed = YAML.parse(candidate.content);
		} catch (err) {
			logger.warn("Compaction config: failed to parse YAML", { path: candidate.path, error: String(err) });
			continue;
		}
		if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
			logger.warn("Compaction config: expected a YAML mapping", { path: candidate.path });
			continue;
		}

		const result = compactionConfigSchema(parsed);
		if (result instanceof type.errors) {
			logger.warn("Compaction config: invalid schema", { path: candidate.path, error: result.summary });
			continue;
		}

		const configuredPrompts = compactionConfigShape.assert(result).prompts;
		if (!configuredPrompts) continue;
		for (const [name, value] of Object.entries(configuredPrompts)) {
			const expanded = await expandAtImports(value, candidate.path, { readFile: readLivePromptFile });
			// Validate like every other config error in this loader: a template
			// that cannot compile or render is logged and skipped here, instead of
			// aborting compaction or handoff at render time when the session may
			// already be near the context window. Render truthy and falsey sample
			// contexts so both sides of optional guards are exercised.
			try {
				prompt.validate(expanded);
				const render = prompt.compile(expanded);
				for (const context of TEMPLATE_VALIDATION_CONTEXTS) render(context);
			} catch (error) {
				logger.warn("Compaction config: invalid prompt template", {
					path: candidate.path,
					key: name,
					error: error instanceof Error ? error.message : String(error),
				});
				continue;
			}
			prompts[name as keyof CompactionPromptTemplates] = expanded;
		}
	}

	return { prompts };
}

/**
 * Uncached reader for `@`-imported prompt files. Discovery runs again at each
 * one-off operation, so imports must observe live edits instead of the
 * process-wide capability read cache.
 */
async function readLivePromptFile(absPath: string): Promise<string | null> {
	try {
		if (!(await fs.stat(absPath)).isFile()) return null;
		return await Bun.file(absPath).text();
	} catch {
		return null;
	}
}
