import { describe, expect, test } from "bun:test";
import { buildModel } from "@oh-my-pi/pi-catalog/build";
import { streamCursor } from "../src/providers/cursor";
import type { AssistantMessage, Context, Model, ProviderSessionState } from "../src/types";
import { e2eApiKey, resolveApiKey } from "./oauth";
import { fetchCursorUsableModels } from "@oh-my-pi/pi-catalog/discovery/cursor";

const token = (await resolveApiKey("cursor")) ?? e2eApiKey("CURSOR_ACCESS_TOKEN");
const liveEnabled = Bun.env.CI === undefined && token !== undefined && token !== "";

const PNG_BASE64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9WlKz9sAAAAASUVORK5CYII=";

const model: Model<"cursor-agent"> = buildModel({
	id: "gemini-3.7-flash",
	name: "Gemini 3.7 Flash",
	provider: "cursor",
	api: "cursor-agent",
	baseUrl: "https://api2.cursor.sh",
	reasoning: true,
	input: ["text", "image"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 1_000_000,
	maxTokens: 64_000,
});

const maxModel: Model<"cursor-agent"> = buildModel({
	...model,
	id: "gpt-5.6-sol-1m",
	name: "GPT-5.6 Sol Max",
	contextWindow: 1_000_000,
	cursorMaxMode: true,
	cursorContext: "1m",
});

const tool = {
	name: "inspect_pixel",
	description: "Return a tiny image for inspection.",
	parameters: {
		type: "object",
		properties: {},
		additionalProperties: false,
	},
} as const;

async function collect(
	context: Context,
	sessionId: string,
	maxTokens: number,
	providerSessionState?: Map<string, ProviderSessionState>,
): Promise<{
	readonly result: AssistantMessage;
	readonly thinking: string;
	readonly argumentDeltas: number;
}> {
	if (token === undefined || token === "") throw new Error("Cursor live token is required");
	const stream = streamCursor(model, context, {
		apiKey: token,
		sessionId,
		wireModelId: "gemini-3.7-flash-high",
		maxTokens,
		providerSessionState,
	});
	let thinking = "";
	let argumentDeltas = 0;
	for await (const event of stream) {
		if (event.type === "thinking_delta") thinking += event.delta;
		if (event.type === "toolcall_delta") argumentDeltas++;
	}
	return { result: await stream.result(), thinking, argumentDeltas };
}

function visibleText(message: AssistantMessage): string {
	return message.content.flatMap(part => (part.type === "text" ? [part.text] : [])).join("");
}

function expectSuccess(message: AssistantMessage): void {
	if (message.stopReason === "error") {
		throw new Error(`Cursor live inference failed: ${message.errorMessage ?? "unknown provider error"}`);
	}
	expect(message.stopReason).toBe("stop");
}

function closeProviderState(state: Map<string, ProviderSessionState>): void {
	for (const value of state.values()) value.close();
	state.clear();
}

describe.skipIf(!liveEnabled)("Cursor managed inference live", () => {
	test(
		"joins authoritative context and Max Mode metadata from all three catalog surfaces",
		async () => {
			if (token === undefined || token === "") throw new Error("Cursor live token is required");
			const models = await fetchCursorUsableModels({ apiKey: token });
			if (models === null) throw new Error("Cursor live catalog fetch failed");
			expect(models.find(candidate => candidate.id === "gemini-3.7-flash")).toMatchObject({
				contextWindow: 1_000_000,
				cursorMaxMode: false,
			});
			expect(models.find(candidate => candidate.id === "cursor-grok-4.6")).toMatchObject({
				contextWindow: 256_000,
				cursorMaxMode: false,
			});
			expect(models.find(candidate => candidate.id === "gpt-5.6-sol")).toMatchObject({
				contextWindow: 272_000,
				cursorContext: "272k",
				cursorMaxMode: false,
			});
			expect(models.find(candidate => candidate.id === "gpt-5.6-sol-1m")).toMatchObject({
				contextWindow: 1_000_000,
				cursorContext: "1m",
				cursorMaxMode: true,
			});
		},
		{ timeout: 120_000 },
	);

	test(
		"runs the distinct 1M Max route with one authoritative answer",
		async () => {
			if (token === undefined || token === "") throw new Error("Cursor live token is required");
			const stream = streamCursor(
				maxModel,
				{
					messages: [
						{
							role: "user",
							content: "Reply exactly CURSOR_MAX_OK.",
							timestamp: Date.now(),
						},
					],
				},
				{
					apiKey: token,
					sessionId: `omp-cursor-max-${crypto.randomUUID()}`,
					wireModelId: "gpt-5.6-sol-high",
					maxTokens: 256,
				},
			);
			for await (const _event of stream) {
				// Drain the complete paid response before asserting its final form.
			}
			const result = await stream.result();
			expectSuccess(result);
			expect(result.model).toBe("gpt-5.6-sol-1m");
			expect(visibleText(result)).toBe("CURSOR_MAX_OK");
		},
		{ retry: 1, timeout: 120_000 },
	);

	test(
		"retains visible thinking and one authoritative final answer",
		async () => {
			const { result, thinking } = await collect(
				{
					messages: [
						{
							role: "user",
							content: "Reason about whether 37 is prime, then reply with exactly CURSOR_LIVE_OK.",
							timestamp: Date.now(),
						},
					],
				},
				`omp-cursor-thinking-${crypto.randomUUID()}`,
				1_024,
			);
			expectSuccess(result);
			expect(thinking.trim().length).toBeGreaterThan(0);
			expect(result.content.some(part => part.type === "thinking" && part.thinking.includes(thinking))).toBe(true);
			expect(visibleText(result)).toBe("CURSOR_LIVE_OK");
		},
		{ retry: 1, timeout: 120_000 },
	);

	test(
		"accepts an extracted-source user image part",
		async () => {
			const { result } = await collect(
				{
					messages: [
						{
							role: "user",
							content: [
								{ type: "text", text: "If the attached image decodes, reply exactly CURSOR_IMAGE_OK." },
								{ type: "image", data: PNG_BASE64, mimeType: "image/png" },
							],
							timestamp: Date.now(),
						},
					],
				},
				`omp-cursor-image-${crypto.randomUUID()}`,
				512,
			);
			expectSuccess(result);
			expect(visibleText(result)).toBe("CURSOR_IMAGE_OK");
		},
		{ retry: 1, timeout: 120_000 },
	);

	test(
		"continues after an ordinary tool result carrying an image",
		async () => {
			const sessionId = `omp-cursor-image-tool-${crypto.randomUUID()}`;
			const providerSessionState = new Map<string, ProviderSessionState>();
			const prompt = "Call inspect_pixel with an empty object. Do not answer before the tool result.";
			try {
				const first = await collect(
					{
						messages: [{ role: "user", content: prompt, timestamp: Date.now() }],
						tools: [tool],
					},
					sessionId,
					512,
					providerSessionState,
				);
				expect(first.result.stopReason).toBe("toolUse");
				const calls = first.result.content.filter(part => part.type === "toolCall");
				expect(calls).toHaveLength(1);
				expect(calls[0]).toMatchObject({ name: tool.name, arguments: {} });
				expect(first.argumentDeltas).toBeGreaterThan(0);
				const call = calls[0];
				if (call === undefined) throw new Error("Cursor live tool call is missing");

				const continuation = await collect(
					{
						messages: [
							{ role: "user", content: prompt, timestamp: first.result.timestamp - 1 },
							first.result,
							{
								role: "toolResult",
								toolCallId: call.id,
								toolName: call.name,
								content: [
									{
										type: "text",
										text: "The tool returned a valid one-pixel PNG. Reply exactly CURSOR_IMAGE_TOOL_OK.",
									},
									{ type: "image", data: PNG_BASE64, mimeType: "image/png" },
								],
								isError: false,
								timestamp: Date.now(),
							},
						],
						tools: [tool],
					},
					sessionId,
					512,
					providerSessionState,
				);
				expectSuccess(continuation.result);
				expect(visibleText(continuation.result)).toBe("CURSOR_IMAGE_TOOL_OK");
			} finally {
				closeProviderState(providerSessionState);
			}
		},
		{ retry: 1, timeout: 180_000 },
	);
});
