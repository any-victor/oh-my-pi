import { afterEach, describe, expect, test } from "bun:test";
import type { Http2Server, ServerHttp2Session, ServerHttp2Stream } from "node:http2";
import { createServer } from "node:http2";
import type {
	InferenceStreamRequest,
	InferenceStreamResponse,
	RunInferenceServerMessage,
} from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import {
	InferenceExtendedUsageInfoSchema,
	InferenceModelConfigSchema,
	InferenceRequestedModelSchema,
	InferenceResponseInfoSchema,
	InferenceResponseMessageSchema,
	InferenceStreamRequestSchema,
	InferenceStreamResponseSchema,
	InferenceTextStreamPartSchema,
	RunInferenceClientMessageSchema,
	RunInferenceInvocationEndSchema,
	RunInferenceInvocationResponseSchema,
	RunInferenceRunReadySchema,
	RunInferenceServerMessageSchema,
} from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import { buildModel } from "@oh-my-pi/pi-catalog/build";
import { create, fromBinary, toBinary } from "@oh-my-pi/pi-catalog/discovery/protobuf";
import { CONNECT_FLAG_END_STREAM, ConnectFrameDecoder, encodeConnectFrame } from "../src/providers/cursor/connect";
import { type CursorOptions, streamCursor } from "../src/providers/cursor";
import type { AssistantMessage, ProviderSessionState } from "../src/types";
import { AssistantMessageEventStream } from "../src/utils/event-stream";

let server: Http2Server | undefined;
const sessions = new Set<ServerHttp2Session>();

afterEach(() => {
	for (const session of sessions) session.destroy();
	sessions.clear();
	server?.close();
	server = undefined;
});

function send(stream: ServerHttp2Stream, message: RunInferenceServerMessage): void {
	stream.write(encodeConnectFrame(toBinary(RunInferenceServerMessageSchema, message)));
}

async function loopback(): Promise<{
	readonly origin: string;
	readonly runRequests: () => number;
	readonly invocations: () => number;
	readonly invokeMaxTokens: () => number[];
	readonly headers: () => Record<string, string> | undefined;
}> {
	let runRequests = 0;
	let invocations = 0;
	let capturedHeaders: Record<string, string> | undefined;
	const invokeMaxTokens: number[] = [];
	server = createServer();
	server.on("session", session => sessions.add(session));
	server.on("stream", (stream: ServerHttp2Stream, headers) => {
		capturedHeaders = Object.fromEntries(Object.entries(headers).map(([name, value]) => [name, String(value)]));
		stream.respond({ ":status": 200, "content-type": "application/connect+proto", "x-loopback": "ok" });
		const decoder = new ConnectFrameDecoder();
		stream.on("data", (chunk: Uint8Array) => {
			for (const frame of decoder.push(chunk)) {
				const message = fromBinary(RunInferenceClientMessageSchema, frame.body);
				if (message.message.case === "runRequest") {
					runRequests++;
					send(
						stream,
						create(RunInferenceServerMessageSchema, {
							message: {
								case: "runReady",
								value: create(RunInferenceRunReadySchema, {
									resolvedModel: create(InferenceRequestedModelSchema, { modelId: "composer-2.5" }),
								}),
							},
						}),
					);
				}
				if (message.message.case === "invokeModel") {
					invocations++;
					const { invocationId, request } = message.message.value;
					invokeMaxTokens.push(request?.modelConfig?.maxTokens ?? 0);
					const response = (value: Partial<InferenceStreamResponse>) =>
						create(RunInferenceServerMessageSchema, {
							message: {
								case: "invocationResponse",
								value: create(RunInferenceInvocationResponseSchema, {
									invocationId,
									response: create(InferenceStreamResponseSchema, value),
								}),
							},
						});
					send(
						stream,
						response({
							response: {
								case: "textPart",
								value: create(InferenceTextStreamPartSchema, {
									text: `stream-${invocations}`,
									isFinal: false,
								}),
							},
						}),
					);
					send(
						stream,
						response({
							response: {
								case: "textPart",
								value: create(InferenceTextStreamPartSchema, {
									text: `stream-${invocations}`,
									isFinal: true,
								}),
							},
						}),
					);
					send(
						stream,
						response({
							response: {
								case: "responseInfo",
								value: create(InferenceResponseInfoSchema, {
									id: `response-${invocations}`,
									model: "composer-2.5",
									messages: [
										create(InferenceResponseMessageSchema, {
											content: `final-${invocations}`,
										}),
									],
								}),
							},
						}),
					);
					send(
						stream,
						response({
							response: {
								case: "extendedUsage",
								value: create(InferenceExtendedUsageInfoSchema, {
									inputTokens: 4,
									outputTokens: 2,
									cacheReadTokens: 1,
								}),
							},
						}),
					);
					send(
						stream,
						create(RunInferenceServerMessageSchema, {
							message: {
								case: "invocationEnd",
								value: create(RunInferenceInvocationEndSchema, { invocationId }),
							},
						}),
					);
				}
				if (message.message.case === "finishRun") {
					stream.end(encodeConnectFrame(new TextEncoder().encode("{}"), CONNECT_FLAG_END_STREAM));
				}
			}
		});
	});
	const listening = Promise.withResolvers<void>();
	server.listen(0, "127.0.0.1", listening.resolve);
	await listening.promise;
	const address = server.address();
	if (address === null || typeof address === "string") throw new Error("loopback has no port");
	return {
		origin: `http://127.0.0.1:${address.port}`,
		runRequests: () => runRequests,
		invocations: () => invocations,
		invokeMaxTokens: () => invokeMaxTokens,
		headers: () => capturedHeaders,
	};
}

function model(baseUrl: string) {
	return buildModel({
		id: "composer-2.5",
		name: "Composer 2.5",
		provider: "cursor",
		api: "cursor-agent",
		baseUrl,
		reasoning: true,
		input: ["text", "image"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 200_000,
		maxTokens: 64_000,
	});
}

async function collect(stream: AssistantMessageEventStream): Promise<{
	readonly events: string[];
	readonly result: AssistantMessage;
}> {
	const events: string[] = [];
	for await (const event of stream) events.push(event.type);
	return { events, result: await stream.result() };
}

function closeProviderState(state: Map<string, ProviderSessionState>): void {
	for (const value of state.values()) value.close();
	state.clear();
}

describe("Cursor provider entrypoint", () => {
	test("surfaces missing credentials through the provider stream", async () => {
		const stream = streamCursor(
			model("http://127.0.0.1:1"),
			{ messages: [{ role: "user", content: "hello", timestamp: 1 }] },
			{},
		);
		const { events, result } = await collect(stream);
		expect(events).toEqual(["start", "error"]);
		expect(result).toMatchObject({ stopReason: "error", errorMessage: expect.stringContaining("Cursor API key") });
	});

	test("applies hooks and caller headers while reusing one session-scoped run", async () => {
		const target = await loopback();
		const providerSessionState = new Map<string, ProviderSessionState>();
		const responses: number[] = [];
		const options: CursorOptions = {
			apiKey: "HEADER.PAYLOAD.SIGNATURE",
			sessionId: "omp-session",
			providerSessionState,
			headers: { Authorization: "attacker", Connection: "close", "x-caller": "kept" },
			onPayload: (payload: unknown) => {
				const request = payload as InferenceStreamRequest;
				return create(InferenceStreamRequestSchema, {
					...request,
					modelConfig: create(InferenceModelConfigSchema, { maxTokens: 321 }),
				});
			},
			onResponse: (metadata: { readonly status: number }) => {
				responses.push(metadata.status);
			},
		};
		try {
			const first = await collect(
				streamCursor(
					model(target.origin),
					{ messages: [{ role: "user", content: "first", timestamp: 1 }] },
					options,
				),
			);
			const second = await collect(
				streamCursor(
					model(target.origin),
					{ messages: [{ role: "user", content: "second", timestamp: 2 }] },
					options,
				),
			);
			expect(first.events).toEqual(["start", "text_start", "text_delta", "text_end", "done"]);
			expect(first.result).toMatchObject({
				content: [{ type: "text", text: "final-1" }],
				responseId: "response-1",
				usage: { input: 4, output: 2, cacheRead: 1, totalTokens: 7 },
				stopReason: "stop",
			});
			expect(second.result.content).toEqual([{ type: "text", text: "final-2" }]);
			expect(target.runRequests()).toBe(1);
			expect(target.invocations()).toBe(2);
			expect(target.invokeMaxTokens()).toEqual([321, 321]);
			expect(responses).toEqual([200, 200]);
			expect(target.headers()).toMatchObject({
				authorization: "Bearer HEADER.PAYLOAD.SIGNATURE",
				"x-caller": "kept",
			});
			expect(target.headers()?.connection).toBeUndefined();
		} finally {
			closeProviderState(providerSessionState);
		}
	});

	test("replaces the session runtime when the credential changes", async () => {
		const target = await loopback();
		const providerSessionState = new Map<string, ProviderSessionState>();
		try {
			const context = { messages: [{ role: "user" as const, content: "rotate", timestamp: 1 }] };
			await collect(
				streamCursor(model(target.origin), context, {
					apiKey: "FIRST.PAYLOAD.SIGNATURE",
					sessionId: "omp-session",
					providerSessionState,
				}),
			);
			await collect(
				streamCursor(model(target.origin), context, {
					apiKey: "SECOND.PAYLOAD.SIGNATURE",
					sessionId: "omp-session",
					providerSessionState,
				}),
			);
			expect(target.runRequests()).toBe(2);
			expect(target.invocations()).toBe(2);
			expect(target.headers()?.authorization).toBe("Bearer SECOND.PAYLOAD.SIGNATURE");
		} finally {
			closeProviderState(providerSessionState);
		}
	});
});
