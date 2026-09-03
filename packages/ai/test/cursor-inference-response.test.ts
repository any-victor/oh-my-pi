import { describe, expect, test } from "bun:test";
import type { InferenceStreamResponse, RunInferenceServerMessage } from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import {
	InferenceExtendedUsageInfoSchema,
	InferenceMessageRole,
	InferenceReasoningPartSchema,
	InferenceResponseInfoSchema,
	InferenceResponseMessageSchema,
	InferenceStreamErrorSchema,
	InferenceStreamErrorType,
	InferenceStreamResponseSchema,
	InferenceTextStreamPartSchema,
	InferenceThinkingStreamPartSchema,
	InferenceToolCallStreamPartSchema,
	RunInferenceInvocationResponseSchema,
	RunInferenceServerMessageSchema,
} from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import { create } from "@oh-my-pi/pi-catalog/discovery/protobuf";
import type { AssistantMessage, AssistantMessageEvent } from "../src/types";
import { type InferenceMapperResult, CursorInferenceMapper } from "../src/providers/cursor/response";
import { AssistantMessageEventStream } from "../src/utils/event-stream";

const TOOL = "join_fragments";

function output(): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: "cursor-agent",
		provider: "cursor",
		model: "composer-2.5",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: 1,
	};
}

function response(value: Partial<InferenceStreamResponse>): RunInferenceServerMessage {
	return create(RunInferenceServerMessageSchema, {
		message: {
			case: "invocationResponse",
			value: create(RunInferenceInvocationResponseSchema, {
				invocationId: "invocation",
				response: create(InferenceStreamResponseSchema, value),
			}),
		},
	});
}

async function map(messages: readonly RunInferenceServerMessage[]): Promise<{
	readonly result: AssistantMessage;
	readonly terminal: InferenceMapperResult;
	readonly events: AssistantMessageEvent[];
}> {
	const stream = new AssistantMessageEventStream();
	const result = output();
	const mapper = new CursorInferenceMapper(stream, result, new Set([TOOL]), "invocation", () => undefined);
	const events: AssistantMessageEvent[] = [];
	const collecting = (async () => {
		for await (const event of stream) events.push(event);
	})();
	for (const message of messages) mapper.handle(message);
	const terminal = mapper.finish();
	stream.end();
	await collecting;
	return { result, terminal, events };
}

describe("Cursor managed-inference response", () => {
	test("emits genuine argument deltas and one authoritative tool call", async () => {
		const parts = [
			{ toolCallId: "tool-1", toolName: TOOL, args: "", isComplete: false },
			{ toolCallId: "tool-1", args: '{"left":"A', isComplete: false },
			{ toolCallId: "tool-1", args: '","right":"B"}', isComplete: false },
			{ toolCallId: "tool-1", toolName: TOOL, args: '{"left":"A","right":"B"}', isComplete: true },
		].map(part =>
			response({
				response: { case: "toolCallPart", value: create(InferenceToolCallStreamPartSchema, part) },
			}),
		);
		const { result, terminal, events } = await map(parts);
		expect(events.filter(({ type }) => type === "toolcall_start")).toHaveLength(1);
		expect(events.flatMap(event => (event.type === "toolcall_delta" ? [event.delta] : []))).toEqual([
			'{"left":"A',
			'","right":"B"}',
		]);
		expect(terminal.stopReason).toBe("toolUse");
		expect(result.content).toEqual([
			{ type: "toolCall", id: "tool-1", name: TOOL, arguments: { left: "A", right: "B" } },
		]);
	});

	test("rejects a final response that drops a completed streamed tool", async () => {
		const stream = new AssistantMessageEventStream();
		const result = output();
		const mapper = new CursorInferenceMapper(stream, result, new Set([TOOL]), "invocation", () => undefined);
		mapper.handle(
			response({
				response: {
					case: "toolCallPart",
					value: create(InferenceToolCallStreamPartSchema, {
						toolCallId: "tool-1",
						toolName: TOOL,
						args: "{}",
						isComplete: true,
					}),
				},
			}),
		);
		mapper.handle(
			response({
				response: {
					case: "responseInfo",
					value: create(InferenceResponseInfoSchema, {
						messages: [
							create(InferenceResponseMessageSchema, {
								role: InferenceMessageRole.ASSISTANT,
								content: "tool omitted",
							}),
						],
					}),
				},
			}),
		);
		expect(() => mapper.finish()).toThrow("Cursor final response tool set disagrees");
		stream.end();
	});

	test("preserves streamed thinking when final reasoning is redacted", async () => {
		const { result } = await map([
			response({
				response: {
					case: "thinkingPart",
					value: create(InferenceThinkingStreamPartSchema, { text: "streamed analysis", isFinal: true }),
				},
			}),
			response({
				response: {
					case: "textPart",
					value: create(InferenceTextStreamPartSchema, { text: "draft", isFinal: true }),
				},
			}),
			response({
				response: {
					case: "responseInfo",
					value: create(InferenceResponseInfoSchema, {
						id: "response-1",
						model: "cursor-grok-4.6-high",
						createdAt: 1234n,
						messages: [
							create(InferenceResponseMessageSchema, {
								role: InferenceMessageRole.ASSISTANT,
								content: "final answer",
								reasoningParts: [
									create(InferenceReasoningPartSchema, {
										isRedacted: true,
										redactedData: "opaque",
									}),
								],
							}),
						],
					}),
				},
			}),
		]);
		expect(result.content).toEqual([
			{ type: "thinking", thinking: "streamed analysis" },
			{ type: "redactedThinking", data: "opaque" },
			{ type: "text", text: "final answer" },
		]);
		expect(result).toMatchObject({
			responseId: "response-1",
			upstreamModel: "cursor-grok-4.6-high",
			timestamp: 1234,
		});
	});

	test("gives extended usage precedence", async () => {
		const { result } = await map([
			response({
				response: {
					case: "extendedUsage",
					value: create(InferenceExtendedUsageInfoSchema, {
						inputTokens: 10,
						outputTokens: 4,
						cacheReadTokens: 3,
						cacheWriteTokens: 2,
					}),
				},
			}),
		]);
		expect(result.usage).toMatchObject({ input: 10, output: 4, cacheRead: 3, cacheWrite: 2, totalTokens: 19 });
	});

	test("deduplicates repeated final answer copies", async () => {
		const { result } = await map([
			response({
				response: {
					case: "responseInfo",
					value: create(InferenceResponseInfoSchema, {
						messages: [
							create(InferenceResponseMessageSchema, {
								role: InferenceMessageRole.ASSISTANT,
								content: "answer",
							}),
							create(InferenceResponseMessageSchema, {
								role: InferenceMessageRole.ASSISTANT,
								reasoningParts: [create(InferenceReasoningPartSchema, { signature: "opaque" })],
							}),
							create(InferenceResponseMessageSchema, {
								role: InferenceMessageRole.ASSISTANT,
								content: "answer",
							}),
						],
					}),
				},
			}),
		]);
		expect(result.content).toEqual([
			{ type: "thinking", thinking: "", thinkingSignature: "opaque" },
			{ type: "text", text: "answer" },
		]);
	});

	test("suppresses a repeated text frame after opaque thinking", async () => {
		const { result, events } = await map([
			response({
				response: {
					case: "textPart",
					value: create(InferenceTextStreamPartSchema, { text: "answer", isFinal: true }),
				},
			}),
			response({
				response: {
					case: "thinkingPart",
					value: create(InferenceThinkingStreamPartSchema, { signature: "opaque", isFinal: true }),
				},
			}),
			response({
				response: {
					case: "textPart",
					value: create(InferenceTextStreamPartSchema, { text: "answer", isFinal: false }),
				},
			}),
		]);
		expect(result.content).toEqual([{ type: "text", text: "answer" }]);
		expect(events.filter(event => event.type === "text_delta")).toHaveLength(1);
	});

	test("suppresses a cumulative final copy of an open text block", async () => {
		const { result, events } = await map([
			response({
				response: {
					case: "textPart",
					value: create(InferenceTextStreamPartSchema, { text: "answer", isFinal: false }),
				},
			}),
			response({
				response: {
					case: "textPart",
					value: create(InferenceTextStreamPartSchema, { text: "answer", isFinal: true }),
				},
			}),
		]);
		expect(result.content).toEqual([{ type: "text", text: "answer" }]);
		expect(events.filter(event => event.type === "text_delta")).toHaveLength(1);
	});

	test("maps structured authentication failures for credential rotation", async () => {
		const { terminal } = await map([
			response({
				response: {
					case: "error",
					value: create(InferenceStreamErrorSchema, {
						message: "expired",
						errorType: InferenceStreamErrorType.AUTHENTICATION,
					}),
				},
			}),
		]);
		expect(terminal).toEqual({ stopReason: "error", errorMessage: "expired", errorStatus: 401 });
	});
});
