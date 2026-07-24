import { afterEach, beforeEach, describe, expect, it, vi } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { CompactionPrompts } from "@oh-my-pi/pi-agent-core/compaction";
import { logger } from "@oh-my-pi/pi-utils";
import { discoverCompactionConfig } from "../config";

describe("discoverCompactionConfig", () => {
	let tmp: string;
	let agentDir: string;

	beforeEach(async () => {
		tmp = await fs.mkdtemp(path.join(os.tmpdir(), "omp-compaction-config-"));
		agentDir = await fs.mkdtemp(path.join(os.tmpdir(), "omp-compaction-agentdir-"));
	});

	afterEach(async () => {
		await fs.rm(tmp, { recursive: true, force: true });
		await fs.rm(agentDir, { recursive: true, force: true });
	});

	it("returns empty overrides when no config files exist", async () => {
		expect(await discoverCompactionConfig(tmp, agentDir)).toEqual({ prompts: {} });
	});

	it("merges user and project files by prompt field, with leaf values taking precedence", async () => {
		const project = path.join(tmp, "project");
		const leaf = path.join(project, "leaf");
		await fs.mkdir(path.join(leaf, ".omp"), { recursive: true });
		await Bun.write(
			path.join(agentDir, "COMPACTION.yml"),
			["prompts:", "  summary: user summary", "  turnPrefix: user turn prefix"].join("\n"),
		);
		await Bun.write(
			path.join(project, "COMPACTION.yml"),
			["prompts:", "  summary: project summary", "  handoffDocument: project handoff document"].join("\n"),
		);
		await Bun.write(
			path.join(leaf, ".omp", "COMPACTION.yaml"),
			["prompts:", "  summary: leaf summary", "  fileOperations: leaf file operations"].join("\n"),
		);

		expect((await discoverCompactionConfig(leaf, agentDir)).prompts).toEqual({
			summary: "leaf summary",
			turnPrefix: "user turn prefix",
			handoffDocument: "project handoff document",
			fileOperations: "leaf file operations",
		});
	});

	it("expands @ imports in each configured prompt relative to its source file", async () => {
		await Bun.write(path.join(tmp, "summary.md"), "Imported summary prompt");
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: '@summary.md'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts.summary).toBe("Imported summary prompt");
	});

	it("re-reads @ imports on every discovery", async () => {
		await Bun.write(path.join(tmp, "summary.md"), "v1");
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: '@summary.md'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts.summary).toBe("v1");

		await Bun.write(path.join(tmp, "summary.md"), "v2");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts.summary).toBe("v2");
	});

	it("skips malformed YAML", async () => {
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts: [unclosed");

		expect(await discoverCompactionConfig(tmp, agentDir)).toEqual({ prompts: {} });
	});

	it("skips files with wrong prompt values", async () => {
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: 42");

		expect(await discoverCompactionConfig(tmp, agentDir)).toEqual({ prompts: {} });
	});

	it("skips files with unknown prompt keys", async () => {
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  unknownPrompt: ignored");

		expect(await discoverCompactionConfig(tmp, agentDir)).toEqual({ prompts: {} });
	});

	it("skips invalid imported templates with a warning, keeps valid siblings, and falls back at render time", async () => {
		const importedTemplate = path.join(tmp, "broken.md");
		await Bun.write(importedTemplate, "{{#missingBlock conversation}}body{{/missingBlock}}");
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			["prompts:", "  summary: '@broken.md'", '  handoffContext: "<context>{{document}}</context>"'].join("\n"),
		);

		const warnSpy = vi.spyOn(logger, "warn").mockImplementation(() => {});
		const config = await discoverCompactionConfig(tmp, agentDir);

		expect(config.prompts).toEqual({ handoffContext: "<context>{{document}}</context>" });
		expect(warnSpy).toHaveBeenCalledWith(
			"Compaction config: invalid prompt template",
			expect.objectContaining({ path: path.join(tmp, "COMPACTION.yml"), key: "summary", error: expect.any(String) }),
		);
		expect(() =>
			new CompactionPrompts(config.prompts).render("summary", { conversation: "conversation" }),
		).not.toThrow();
		expect(new CompactionPrompts(config.prompts).render("handoffContext", { document: "document" })).toBe(
			"<context>document</context>",
		);
		warnSpy.mockRestore();
	});

	it("skips templates that become invalid through @ imports", async () => {
		await Bun.write(path.join(tmp, "broken.md"), "{{#each items}}no closing tag");
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: '@broken.md'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips templates that invoke unknown helpers", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			["prompts:", '  summary: "{{missingHelper conversation}}"', "  turnPrefix: valid prefix"].join("\n"),
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({ turnPrefix: "valid prefix" });
	});

	it("accepts documented-field path sections while rejecting unknown helper blocks", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			[
				"prompts:",
				'  summary: "{{#additionalContext}}{{this}}{{/additionalContext}}"',
				'  turnPrefix: "{{#missingHelper conversation}}x{{/missingHelper}}"',
			].join("\n"),
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({
			summary: "{{#additionalContext}}{{this}}{{/additionalContext}}",
		});
	});

	it("skips imported templates whose unknown helpers hide inside conditional bodies", async () => {
		await Bun.write(path.join(tmp, "nested.md"), "{{#if conversation}}{{bogusHelper conversation}}{{/if}}");
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: '@nested.md'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips templates whose unknown helpers hide inside falsey branches", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			"prompts:\n  summary: '{{#if promptOverride}}{{promptOverride}}{{else}}{{bogusHelper conversation}}{{/if}}'",
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips helpers hidden behind value-dependent branches", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			'prompts:\n  summary: \'{{#when additionalFocus "==" "deploy"}}{{bogusHelper conversation}}{{/when}}\'',
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips zero-argument subexpression helpers hidden behind value-dependent branches", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			'prompts:\n  summary: \'{{#when additionalFocus "==" "deploy"}}{{#if (bogusHelper)}}x{{/if}}{{/when}}\'',
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips unresolved partials hidden behind value-dependent branches", async () => {
		await Bun.write(
			path.join(tmp, "COMPACTION.yml"),
			'prompts:\n  summary: \'{{#when additionalFocus "==" "deploy"}}{{> missing}}{{/when}}\'',
		);

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips imported templates that use unknown block helpers", async () => {
		await Bun.write(path.join(tmp, "block.md"), "{{#bogusBlock conversation}}body{{/bogusBlock}}");
		await Bun.write(path.join(tmp, "COMPACTION.yml"), "prompts:\n  summary: '@block.md'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({});
	});

	it("skips non-regular config and imported prompt paths", async () => {
		await fs.mkdir(path.join(tmp, "COMPACTION.yml"));
		await fs.mkdir(path.join(tmp, "prompt-dir"));
		await Bun.write(path.join(tmp, "COMPACTION.yaml"), "prompts:\n  summary: '@prompt-dir'");

		expect((await discoverCompactionConfig(tmp, agentDir)).prompts).toEqual({ summary: "@prompt-dir" });
	});
});
