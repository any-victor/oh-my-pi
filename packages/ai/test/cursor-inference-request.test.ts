import { describe, expect, test } from "bun:test";
import { buildModel } from "@oh-my-pi/pi-catalog/build";
import { decodeJsonStruct, decodeJsonValue } from "@oh-my-pi/pi-catalog/discovery/protobuf";
import type { Context, Model, Tool } from "../src/types";
import { buildInferenceRequest, buildInferenceRunRequest, inferenceRoutingKey } from "../src/providers/cursor/request";

function cursorModel(id = "composer-2.5"): Model<"cursor-agent"> {
	return buildModel({
		id,
		name: id,
		provider: "cursor",
		api: "cursor-agent",
		baseUrl: "https://api2.cursor.sh",
		reasoning: true,
		input: ["text", "image"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 200_000,
		maxTokens: 64_000,
	});
}

const TOOL = {
	name: "join_fragments",
	description: "Join two fragments.",
	parameters: {
		type: "object",
		properties: { left: { type: "string" }, right: { type: "string" } },
		required: ["left", "right"],
		additionalProperties: false,
	},
} as const;

function history(): Context {
	return {
		systemPrompt: ["Use the tool."],
		messages: [
			{ role: "user", content: "Join the fragments.", timestamp: 1 },
			{
				role: "assistant",
				api: "openai-responses",
				provider: "openai",
				model: "gpt",
				responseId: "response-1",
				content: [
					{ type: "thinking", thinking: "Use the tool.", thinkingSignature: "sig" },
					{ type: "text", text: "Calling now." },
					{ type: "toolCall", id: "tool-1", name: TOOL.name, arguments: { left: "A", right: "B" } },
				],
				usage: {
					input: 0,
					output: 0,
					cacheRead: 0,
					cacheWrite: 0,
					totalTokens: 0,
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
				},
				stopReason: "toolUse",
				timestamp: 2,
			},
			{
				role: "toolResult",
				toolCallId: "tool-1",
				toolName: TOOL.name,
				content: [{ type: "text", text: "AB" }],
				isError: false,
				timestamp: 3,
			},
		],
		tools: [TOOL],
	};
}

describe("Cursor managed-inference request", () => {
	test("projects complete cross-provider history and ordinary OMP tools", () => {
		const request = buildInferenceRequest(history());
		expect(request.invocationId).toBeUndefined();
		expect(request.conversationId).toBeUndefined();
		expect(request.requestedModel).toBeUndefined();
		expect(request.messages.map(message => message.role)).toEqual([4, 1, 2, 3]);
		const assistant = request.messages[2];
		expect(assistant?.content).toEqual({ case: "text", value: "Calling now." });
		expect(assistant?.modelProviderMessageId).toBe("response-1");
		expect(decodeJsonStruct(assistant?.toolCalls[0]?.args ?? new Uint8Array())).toEqual({ left: "A", right: "B" });
		const result = request.messages[3]?.content;
		if (result?.case !== "toolContent") throw new Error("tool result content missing");
		expect(decodeJsonValue(result.value.parts[0]?.result ?? new Uint8Array())).toBe("AB");
		expect(request.tools[0]).toMatchObject({ name: TOOL.name, description: TOOL.description });
		expect(decodeJsonStruct(request.tools[0]?.parameters ?? new Uint8Array())).toEqual({
			jsonSchema: {
				...TOOL.parameters,
				required: ["left", "right"],
			},
		});
	});

	test("routes on a stable OMP session and resolved model selection", () => {
		const model = cursorModel();
		const run = buildInferenceRunRequest(model, history(), "omp-session");
		expect(run).toMatchObject({
			conversationId: "omp-session",
			agentMode: "agent",
			requestedModel: {
				modelId: "composer-2.5",
				parameters: [expect.objectContaining({ id: "fast", value: "false" })],
			},
			routingConversation: [
				expect.objectContaining({ role: 1, text: "Join the fragments." }),
				expect.objectContaining({ role: 2, text: "Calling now." }),
			],
		});
		expect(inferenceRoutingKey(model)).toBe(
			'{"modelId":"composer-2.5","maxMode":false,"parameters":[{"id":"fast","value":"false"}]}',
		);
	});

	test("maps resolved effort siblings into RunInference model parameters", () => {
		const gpt = cursorModel("gpt-5.6-sol");
		const gptKey = inferenceRoutingKey(gpt, { wireModelId: "gpt-5.6-sol-high" });
		expect(JSON.parse(gptKey)).toEqual({
			modelId: "gpt-5.6-sol",
			maxMode: false,
			parameters: [
				{ id: "context", value: "272k" },
				{ id: "reasoning", value: "high" },
				{ id: "fast", value: "false" },
			],
		});
		const grok = cursorModel("cursor-grok-4.6");
		expect(JSON.parse(inferenceRoutingKey(grok, { wireModelId: "cursor-grok-4.6-xhigh-fast" }))).toEqual({
			modelId: "grok-4.6",
			maxMode: false,
			parameters: [
				{ id: "effort", value: "xhigh" },
				{ id: "fast", value: "true" },
			],
		});
		const gemini = cursorModel("gemini-3.7-flash");
		expect(JSON.parse(inferenceRoutingKey(gemini, { wireModelId: "gemini-3.7-flash-medium" }))).toEqual({
			modelId: "gemini-3.7-flash",
			maxMode: false,
			parameters: [{ id: "effort", value: "medium" }],
		});
		const opus = cursorModel("claude-opus-5-high");
		expect(JSON.parse(inferenceRoutingKey(opus, { wireModelId: "claude-opus-5-thinking-high" }))).toEqual({
			modelId: "claude-opus-5",
			maxMode: false,
			parameters: [
				{ id: "thinking", value: "true" },
				{ id: "context", value: "300k" },
				{ id: "effort", value: "high" },
				{ id: "fast", value: "false" },
			],
		});
	});

	test("forwards request limits and rejects malformed schemas before transport", () => {
		const request = buildInferenceRequest(history(), {
			maxTokens: 2048,
			temperature: 0.25,
			topP: 0.9,
			stopSequences: ["STOP"],
		});
		expect(request.modelConfig).toMatchObject({
			maxTokens: 2048,
			temperature: 0.25,
			topP: 0.9,
			stopSequences: ["STOP"],
		});
		expect(() =>
			buildInferenceRequest({
				messages: [{ role: "user", content: "hello", timestamp: 1 }],
				tools: [{ name: "bad", description: "bad", parameters: "not-an-object" } as unknown as Tool],
			}),
		).toThrow("schema must be a JSON object");
	});
});
