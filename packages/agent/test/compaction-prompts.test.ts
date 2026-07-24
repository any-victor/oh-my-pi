import { describe, expect, it } from "bun:test";
import { CompactionPrompts, DEFAULT_COMPACTION_PROMPT_TEMPLATES } from "@oh-my-pi/pi-agent-core/compaction";
import { prompt } from "@oh-my-pi/pi-utils";
import branchSummaryContext from "../src/compaction/prompts/branch-summary-context.md" with { type: "text" };
import compactionSummaryContext from "../src/compaction/prompts/compaction-summary-context.md" with { type: "text" };
import summarizationSystem from "../src/compaction/prompts/summarization-system.md" with { type: "text" };

describe("CompactionPrompts", () => {
	it("renders configured overrides and falls back to bundled defaults per key", () => {
		const prompts = new CompactionPrompts({ handoffContext: "<hc>{{document}}</hc>" });
		expect(prompts.render("handoffContext", { document: "doc" })).toBe("<hc>doc</hc>");
		// Unrelated key keeps the bundled default.
		expect(prompts.get("turnPrefix")).toBe(DEFAULT_COMPACTION_PROMPT_TEMPLATES.turnPrefix);
	});

	it("renders helpers against real values and keeps raw text byte-exact", () => {
		const prompts = new CompactionPrompts({
			turnPrefix: "{{len conversation}}:{{conversation}}",
		});
		const conversation = "| a  |\n\n\n| b |";
		expect(prompts.render("turnPrefix", { conversation })).toBe(`${conversation.length}:${conversation}`);
	});

	it("is a snapshot: mutating the source object after construction changes nothing", () => {
		const source = { handoffContext: "v1 {{document}}" };
		const prompts = new CompactionPrompts(source);
		source.handoffContext = "v2 {{document}}";
		expect(prompts.render("handoffContext", { document: "d" })).toBe("v1 d");
	});

	it("treats an empty-string override as configured, not missing", () => {
		const prompts = new CompactionPrompts({ branchSummaryPreamble: "" });
		expect(prompts.render("branchSummaryPreamble", {})).toBe("");
	});
	it("preserves the pre-template bytes for frozen default prompts", () => {
		const prompts = new CompactionPrompts();

		expect(prompts.render("summarizationSystem", {})).toBe(prompt.render(summarizationSystem));
		expect(prompts.render("compactionSummaryContext", { summary: "summary" })).toBe(
			prompt.render(compactionSummaryContext, { summary: "summary" }),
		);
		expect(prompts.render("branchSummaryContext", { summary: "summary" })).toBe(
			prompt.render(branchSummaryContext, { summary: "summary" }),
		);
	});
});
