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
import type { Context, ImageContent, Message, Model, TextContent, Tool } from "../../types";
import { normalizeSystemPrompts } from "../../utils";
import { toolWireSchema } from "../../utils/schema";

export interface CursorInferenceRequestOptions {
	readonly maxTokens?: number;
	readonly temperature?: number;
	readonly topP?: number;
	readonly stopSequences?: readonly string[];
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

export function messageToInference(message: Message): InferenceCoreMessage {
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
							toolCallId: part.id,
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
						toolCallId: message.toolCallId,
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
export function buildInferenceRequest(
	context: Context,
	options: CursorInferenceRequestOptions = {},
): InferenceStreamRequest {
	const messages = context.messages.map(messageToInference);
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
	return create(InferenceStreamRequestSchema, {
		messages,
		tools: context.tools?.map(toolToInference) ?? [],
		modelConfig,
	});
}

interface RequestedModelFields {
	readonly modelId: string;
	readonly parameters: readonly { readonly id: string; readonly value: string }[];
}

const SPECIAL_SELECTIONS: Readonly<Record<string, RequestedModelFields>> = {
	"auto-smart": { modelId: "auto-smart", parameters: [{ id: "optimize_for", value: "balanced" }] },
	"composer-2.5": { modelId: "composer-2.5", parameters: [{ id: "fast", value: "false" }] },
};

function isOpenAiModel(modelId: string): boolean {
	return /^(gpt-|o[1-9](?:-|$)|codex-)/u.test(modelId);
}

function requestedModelFields(modelId: string): RequestedModelFields {
	const captured = SPECIAL_SELECTIONS[modelId];
	if (captured !== undefined) return captured;

	const grok = /^cursor-grok-(4\.6)-(low|medium|high|xhigh)(-fast)?$/u.exec(modelId);
	if (grok?.[1] !== undefined && grok[2] !== undefined) {
		return {
			modelId: `grok-${grok[1]}`,
			parameters: [
				{ id: "effort", value: grok[2] },
				{ id: "fast", value: String(grok[3] === "-fast") },
			],
		};
	}

	const gemini = /^(gemini-3\.7-flash)-(low|medium|high)$/u.exec(modelId);
	if (gemini?.[1] !== undefined && gemini[2] !== undefined) {
		return { modelId: gemini[1], parameters: [{ id: "effort", value: gemini[2] }] };
	}

	const opus = /^(claude-opus-5)-thinking-(low|medium|high|xhigh|max)(-fast)?$/u.exec(modelId);
	if (opus?.[1] !== undefined && opus[2] !== undefined) {
		return {
			modelId: opus[1],
			parameters: [
				{ id: "thinking", value: "true" },
				{ id: "context", value: "300k" },
				{ id: "effort", value: opus[2] },
				{ id: "fast", value: String(opus[3] === "-fast") },
			],
		};
	}

	const match = /^(.*)-(none|minimal|low|medium|high|xhigh|extra-high|max)(-fast)?$/u.exec(modelId);
	if (match?.[1] !== undefined && match[2] !== undefined && isOpenAiModel(match[1])) {
		return {
			modelId: `${match[1]}${match[3] ?? ""}`,
			parameters: [
				{ id: "context", value: "272k" },
				{ id: "reasoning", value: match[2] },
				{ id: "fast", value: String(match[3] === "-fast") },
			],
		};
	}

	return { modelId, parameters: [] };
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
		parameters: requested.parameters.map(parameter => create(InferenceModelParameterValueSchema, parameter)),
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
