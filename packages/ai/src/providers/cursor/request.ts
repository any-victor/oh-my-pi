import type {
	InferenceContentPart,
	InferenceCoreMessage,
	InferenceModelConfig,
	InferenceRequestedModel,
	InferenceStreamRequest,
	RunInferenceRoutingMessage,
	RunInferenceRunRequest,
} from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import {
	InferenceAgentToolSchema,
	InferenceContentPartSchema,
	InferenceContentPartsSchema,
	InferenceCoreMessageSchema,
	InferenceImagePartSchema,
	InferenceMessageRole,
	InferenceModelConfigSchema,
	InferenceModelParameterValueSchema,
	InferenceReasoningPartSchema,
	InferenceRequestedModelSchema,
	InferenceStreamRequestSchema,
	InferenceTextPartSchema,
	InferenceToolCallSchema,
	InferenceToolResultContentSchema,
	InferenceToolResultPartSchema,
	RunInferenceRoutingMessageSchema,
	RunInferenceRoutingRole,
	RunInferenceRunRequestSchema,
} from "@oh-my-pi/pi-catalog/discovery/cursor-proto";
import {
	create,
	encodeJsonStruct,
	encodeJsonValue,
	type JsonObject,
	type JsonValue,
} from "@oh-my-pi/pi-catalog/discovery/protobuf";
import type { Context, ImageContent, Message, Model, TextContent, Tool, ToolChoice } from "../../types";
import { normalizeSystemPrompts, normalizeToolCallId } from "../../utils";
import { toolWireSchema } from "../../utils/schema";
import {
	cursorEffortParameters,
	cursorEffortSuffix,
	cursorModelParameters,
	cursorModelRoute,
} from "@oh-my-pi/pi-catalog/compat/behavior";

export interface CursorInferenceRequestOptions {
	readonly maxTokens?: number;
	readonly temperature?: number;
	readonly topP?: number;
	readonly stopSequences?: readonly string[];
	readonly toolChoice?: ToolChoice;
}

export interface CursorRequestedModelOptions {
	readonly wireModelId?: string;
	readonly maxMode?: boolean;
}

function isJsonValue(value: unknown): value is JsonValue {
	if (value === null || typeof value === "string" || typeof value === "boolean") return true;
	if (typeof value === "number") return Number.isFinite(value);
	if (Array.isArray(value)) return value.every(isJsonValue);
	if (typeof value !== "object" || value === null) return false;
	const prototype = Object.getPrototypeOf(value);
	if (prototype !== Object.prototype && prototype !== null) return false;
	return Object.values(value).every(isJsonValue);
}

function requiredJsonObject(value: unknown, label: string): JsonObject {
	if (!isJsonValue(value) || Array.isArray(value) || value === null || typeof value !== "object") {
		throw new Error(`${label} must be a JSON object`);
	}
	return value;
}

function imagePart(image: ImageContent): InferenceContentPart {
	return create(InferenceContentPartSchema, {
		part: {
			case: "image",
			value: create(InferenceImagePartSchema, { data: image.data, mimeType: image.mimeType }),
		},
	});
}

function textPart(text: string): InferenceContentPart {
	return create(InferenceContentPartSchema, {
		part: { case: "text", value: create(InferenceTextPartSchema, { text }) },
	});
}

function textAndImagesContent(content: string | (TextContent | ImageContent)[]): InferenceCoreMessage["content"] {
	if (typeof content === "string") return { case: "text", value: content };
	if (content.every(part => part.type === "text")) {
		return { case: "text", value: content.map(part => part.text).join("") };
	}
	return {
		case: "parts",
		value: create(InferenceContentPartsSchema, {
			parts: content.map(part => (part.type === "text" ? textPart(part.text) : imagePart(part))),
		}),
	};
}

function toolResultJson(message: Extract<Message, { role: "toolResult" }>): JsonValue {
	const text = message.content.flatMap(part => (part.type === "text" ? [part.text] : []));
	if (text.length === 1) return text[0] ?? "";
	return text.map(value => ({ type: "text", text: value }));
}

function toolResultExperimentalContent(message: Extract<Message, { role: "toolResult" }>): InferenceContentPart[] {
	if (!message.content.some(part => part.type === "image")) return [];
	return message.content.map(part => (part.type === "text" ? textPart(part.text) : imagePart(part)));
}

export function messageToInference(message: Message, toolCallIds: ReadonlyMap<object, string>): InferenceCoreMessage {
	if (message.role === "user" || message.role === "developer") {
		return create(InferenceCoreMessageSchema, {
			role: message.role === "user" ? InferenceMessageRole.USER : InferenceMessageRole.SYSTEM,
			content: textAndImagesContent(message.content),
		});
	}
	if (message.role === "assistant") {
		const visibleParts: (TextContent | ImageContent)[] = [];
		const reasoningParts = [];
		const toolCalls = [];
		for (const part of message.content) {
			switch (part.type) {
				case "text":
				case "image":
					visibleParts.push(part);
					break;
				case "thinking":
					reasoningParts.push(
						create(InferenceReasoningPartSchema, {
							text: part.thinking,
							signature: part.thinkingSignature,
							modelName: message.provider === "cursor" ? message.upstreamModel : undefined,
						}),
					);
					break;
				case "redactedThinking":
					reasoningParts.push(
						create(InferenceReasoningPartSchema, { isRedacted: true, text: "", redactedData: part.data }),
					);
					break;
				case "toolCall": {
					const args = requiredJsonObject(part.arguments, `Cursor inference tool '${part.name}' arguments`);
					toolCalls.push(
						create(InferenceToolCallSchema, {
							toolCallId: toolCallIds.get(part) ?? normalizeToolCallId(part.id),
							toolName: part.name,
							args: encodeJsonStruct(args),
							rawToolCallArgs: JSON.stringify(args),
						}),
					);
					break;
				}
				case "fallback":
				case "anthropicServerTool":
					break;
			}
		}
		const hasImages = visibleParts.some(part => part.type === "image");
		const joinedText = visibleParts.flatMap(part => (part.type === "text" ? [part.text] : [])).join("");
		const content = hasImages
			? {
					case: "parts" as const,
					value: create(InferenceContentPartsSchema, {
						parts: visibleParts.map(part => (part.type === "text" ? textPart(part.text) : imagePart(part))),
					}),
				}
			: joinedText === ""
				? undefined
				: { case: "text" as const, value: joinedText };
		return create(InferenceCoreMessageSchema, {
			role: InferenceMessageRole.ASSISTANT,
			content,
			reasoningParts,
			toolCalls,
			modelProviderMessageId: message.responseId,
		});
	}
	return create(InferenceCoreMessageSchema, {
		role: InferenceMessageRole.TOOL,
		content: {
			case: "toolContent",
			value: create(InferenceToolResultContentSchema, {
				parts: [
					create(InferenceToolResultPartSchema, {
						toolCallId: toolCallIds.get(message) ?? normalizeToolCallId(message.toolCallId),
						toolName: message.toolName,
						result: encodeJsonValue(toolResultJson(message)),
						isError: message.isError,
						experimentalContent: toolResultExperimentalContent(message),
					}),
				],
			}),
		},
	});
}

function toolToInference(tool: Tool) {
	const schema = requiredJsonObject(toolWireSchema(tool), `Cursor inference tool '${tool.name}' schema`);
	return create(InferenceAgentToolSchema, {
		name: tool.name,
		description: tool.description,
		// Cursor's IDE converter wraps the JSON Schema before serializing the Struct.
		parameters: encodeJsonStruct({ jsonSchema: schema }),
	});
}

/** Build the complete per-invocation request. Routing and model selection stay on the outer run. */
function uniqueToolCallIds(context: Context): ReadonlyMap<object, string> {
	const assignments = new Map<object, string>();
	const pending = new Map<string, string[]>();
	const used = new Set<string>();
	const allocate = (rawId: string): string => {
		const normalized = normalizeToolCallId(rawId);
		let candidate = normalized;
		let duplicate = 1;
		while (used.has(candidate)) {
			const suffix = `_dup${duplicate++}`;
			candidate = `${normalized.slice(0, 64 - suffix.length)}${suffix}`;
		}
		used.add(candidate);
		return candidate;
	};
	for (const message of context.messages) {
		if (message.role === "assistant") {
			for (const part of message.content) {
				if (part.type !== "toolCall") continue;
				const assigned = allocate(part.id);
				assignments.set(part, assigned);
				const queue = pending.get(part.id) ?? [];
				queue.push(assigned);
				pending.set(part.id, queue);
			}
			continue;
		}
		if (message.role !== "toolResult") continue;
		const queue = pending.get(message.toolCallId);
		const assigned = queue?.shift() ?? allocate(message.toolCallId);
		assignments.set(message, assigned);
		if (queue?.length === 0) pending.delete(message.toolCallId);
	}
	return assignments;
}

export function buildInferenceRequest(
	context: Context,
	options: CursorInferenceRequestOptions = {},
): InferenceStreamRequest {
	const toolCallIds = uniqueToolCallIds(context);
	const messages = context.messages.map(message => messageToInference(message, toolCallIds));
	for (const prompt of normalizeSystemPrompts(context.systemPrompt).reverse()) {
		messages.unshift(
			create(InferenceCoreMessageSchema, {
				role: InferenceMessageRole.SYSTEM,
				content: { case: "text", value: prompt },
			}),
		);
	}
	const modelConfig: InferenceModelConfig | undefined =
		options.maxTokens === undefined &&
		options.temperature === undefined &&
		options.topP === undefined &&
		options.stopSequences === undefined
			? undefined
			: create(InferenceModelConfigSchema, {
					maxTokens: options.maxTokens,
					temperature: options.temperature,
					topP: options.topP,
					stopSequences: options.stopSequences === undefined ? undefined : [...options.stopSequences],
				});
	const forcedToolName =
		typeof options.toolChoice === "object"
			? options.toolChoice.type === "computer"
				? "computer"
				: "function" in options.toolChoice
					? options.toolChoice.function.name
					: options.toolChoice.name
			: undefined;
	const tools =
		options.toolChoice === "none"
			? []
			: forcedToolName === undefined
				? (context.tools ?? [])
				: (context.tools ?? []).filter(
						tool => tool.name === forcedToolName || tool.customWireName === forcedToolName,
					);
	return create(InferenceStreamRequestSchema, {
		messages,
		tools: tools.map(toolToInference),
		modelConfig,
	});
}

interface RequestedModelFields {
	readonly modelId: string;
	readonly parameters: readonly { readonly id: string; readonly value: string }[];
}

function requestedModelFields(modelId: string): RequestedModelFields {
	const routed = cursorModelRoute(modelId);
	if (routed !== undefined) return routed;

	const effort = cursorEffortSuffix(modelId);
	if (effort !== undefined) {
		return {
			modelId: `${effort.base}${effort.fast ? "-fast" : ""}`,
			parameters: cursorEffortParameters(effort.tier, effort.fast),
		};
	}

	return { modelId, parameters: cursorModelParameters(modelId) };
}

function withCursorContext(
	parameters: readonly { readonly id: string; readonly value: string }[],
	context: string | undefined,
): readonly { readonly id: string; readonly value: string }[] {
	if (context === undefined) return parameters;
	const retained = parameters.filter(parameter => parameter.id !== "context");
	return [{ id: "context", value: context }, ...retained];
}

export function inferenceRequestedModel(
	model: Model<"cursor-agent">,
	options: CursorRequestedModelOptions = {},
): InferenceRequestedModel {
	const selectedId = options.wireModelId ?? model.requestModelId ?? model.id;
	const requested = requestedModelFields(selectedId);
	return create(InferenceRequestedModelSchema, {
		modelId: requested.modelId,
		maxMode: options.maxMode ?? model.cursorMaxMode === true,
		parameters: withCursorContext(requested.parameters, model.cursorContext).map(parameter =>
			create(InferenceModelParameterValueSchema, parameter),
		),
	});
}

export function inferenceRoutingKey(model: Model<"cursor-agent">, options: CursorRequestedModelOptions = {}): string {
	const requested = inferenceRequestedModel(model, options);
	return JSON.stringify({
		modelId: requested.modelId,
		maxMode: requested.maxMode,
		parameters: requested.parameters.map(({ id, value }) => ({ id, value })),
	});
}

function routingText(message: Message): string {
	if (message.role === "user") {
		return typeof message.content === "string"
			? message.content
			: message.content.flatMap(part => (part.type === "text" ? [part.text] : [])).join("");
	}
	if (message.role === "assistant") {
		return message.content.flatMap(part => (part.type === "text" ? [part.text] : [])).join("");
	}
	return "";
}

function routingConversation(context: Context): RunInferenceRoutingMessage[] {
	return context.messages.flatMap(message => {
		if (message.role !== "user" && message.role !== "assistant") return [];
		const text = routingText(message);
		if (text === "") return [];
		return [
			create(RunInferenceRoutingMessageSchema, {
				role: message.role === "user" ? RunInferenceRoutingRole.USER : RunInferenceRoutingRole.ASSISTANT,
				text,
			}),
		];
	});
}

export function buildInferenceRunRequest(
	model: Model<"cursor-agent">,
	context: Context,
	sessionId: string,
	options: CursorRequestedModelOptions = {},
): RunInferenceRunRequest {
	if (sessionId === "") throw new Error("Cursor managed inference requires a stable session id");
	return create(RunInferenceRunRequestSchema, {
		conversationId: sessionId,
		requestedModel: inferenceRequestedModel(model, options),
		routingConversation: routingConversation(context),
		agentMode: "agent",
	});
}
