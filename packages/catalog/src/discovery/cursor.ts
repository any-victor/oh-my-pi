import * as http2 from "node:http2";
import { brotliDecompressSync, gunzipSync } from "node:zlib";
import {
	cursorEffortDisplayLabels,
	cursorEffortLevel,
	cursorEffortPreference,
	cursorEffortTierSuffix,
	cursorModelRoute,
} from "../compat/behavior";
import { classifyModel } from "../compat/taxonomy";
import { Effort, THINKING_EFFORTS } from "../effort";
import { getBundledModels } from "../models";
import { toModelSpec } from "../provider-models/bundled-references";
import type { Model, ModelSpec } from "../types";
import type {
	AvailableModelsResponse,
	AvailableModelsResponse_AvailableModel,
	GetDefaultModelForCliResponse,
	GetUsableModelsResponse,
	ModelDetails,
} from "./cursor-proto";
import {
	AvailableModelsRequestSchema,
	AvailableModelsResponseSchema,
	GetDefaultModelForCliRequestSchema,
	GetDefaultModelForCliResponseSchema,
	GetUsableModelsRequestSchema,
	GetUsableModelsResponseSchema,
} from "./cursor-proto";
import { create, fromBinary, type MessageCodec, type ProtoMessage, toBinary } from "./protobuf";

const CURSOR_DEFAULT_BASE_URL = "https://api2.cursor.sh";
const CURSOR_DEFAULT_CLIENT_VERSION = "cli-2026.09.02-fa0c06e-lab";
const CURSOR_AVAILABLE_MODELS_PATH = "/aiserver.v1.AiService/AvailableModels";
const CURSOR_GET_USABLE_MODELS_PATH = "/agent.v1.AgentService/GetUsableModels";
const CURSOR_GET_DEFAULT_MODEL_PATH = "/agent.v1.AgentService/GetDefaultModelForCli";
const CURSOR_RESPONSE_LIMIT_BYTES = 4 * 1024 * 1024;
const DEFAULT_CONTEXT_WINDOW = 200_000;
const DEFAULT_MAX_TOKENS = 64_000;

/** Options for fetching and joining Cursor's three model-catalog surfaces. */
export interface CursorModelDiscoveryOptions {
	/** Cursor access token used for bearer authentication. */
	apiKey: string;
	/** Optional Cursor API base URL override. */
	baseUrl?: string;
	/** Optional client version override sent as `x-cursor-client-version`. */
	clientVersion?: string;
	/** Optional request timeout in milliseconds. */
	timeoutMs?: number;
	/** Optional list of custom Cursor model ids to include in request context. */
	customModelIds?: string[];
}

interface ModelFamily {
	readonly id: string;
	readonly members: readonly { readonly model: ModelDetails; readonly level: Effort | "off" }[];
}

function buildRequestHeaders(options: CursorModelDiscoveryOptions): Record<string, string> {
	return {
		"content-type": "application/proto",
		te: "trailers",
		authorization: `Bearer ${options.apiKey}`,
		"x-ghost-mode": "false",
		"x-cursor-client-version": options.clientVersion ?? CURSOR_DEFAULT_CLIENT_VERSION,
		"x-cursor-client-type": "cli",
	};
}

function decodeBody(body: Uint8Array, encoding: string | undefined): Uint8Array {
	if (encoding === "gzip") {
		return new Uint8Array(gunzipSync(body, { maxOutputLength: CURSOR_RESPONSE_LIMIT_BYTES }));
	}
	if (encoding === "br") {
		return new Uint8Array(brotliDecompressSync(body, { maxOutputLength: CURSOR_RESPONSE_LIMIT_BYTES }));
	}
	return body;
}

/** HTTP/2 transport required by Cursor API (HTTP/1.1 is rejected with 464). */
async function fetchViaHttp2(
	baseUrl: string,
	path: string,
	body: Uint8Array,
	options: CursorModelDiscoveryOptions,
	timeoutMs: number,
): Promise<Uint8Array | null> {
	const { promise, resolve } = Promise.withResolvers<Uint8Array | null>();
	const client = http2.connect(baseUrl);
	let settled = false;
	const finish = (value: Uint8Array | null): void => {
		if (settled) return;
		settled = true;
		clearTimeout(timer);
		client.close();
		resolve(value);
	};
	const timer = setTimeout(() => {
		client.destroy();
		finish(null);
	}, timeoutMs);
	client.on("error", () => finish(null));

	const request = client.request({ ":method": "POST", ":path": path, ...buildRequestHeaders(options) });
	const chunks: Uint8Array[] = [];
	let responseBytes = 0;
	let encoding: string | undefined;
	request.on("response", headers => {
		const status = Number(headers[":status"] ?? 0);
		encoding = typeof headers["content-encoding"] === "string" ? headers["content-encoding"] : undefined;
		if (status < 200 || status >= 300) finish(null);
	});
	request.on("data", (chunk: Uint8Array) => {
		responseBytes += chunk.byteLength;
		if (responseBytes > CURSOR_RESPONSE_LIMIT_BYTES) {
			request.destroy(new Error("Cursor catalog response exceeded its size limit"));
			finish(null);
			return;
		}
		chunks.push(chunk);
	});
	request.on("end", () => {
		try {
			finish(decodeBody(Buffer.concat(chunks), encoding));
		} catch {
			finish(null);
		}
	});
	request.on("error", () => finish(null));
	request.end(body.byteLength === 0 ? undefined : Buffer.from(body));
	return await promise;
}

function normalizeCustomModelIds(customModelIds: readonly string[] | undefined): string[] {
	if (!customModelIds) return [];
	const normalized = new Set<string>();
	for (const value of customModelIds) {
		const trimmed = value.trim();
		if (trimmed) normalized.add(trimmed);
	}
	return [...normalized];
}

function decodeConnectUnaryBody(payload: Uint8Array): Uint8Array | null {
	if (payload.length < 5) return null;
	let offset = 0;
	while (offset + 5 <= payload.length) {
		const flags = payload[offset];
		const view = new DataView(payload.buffer, payload.byteOffset + offset, payload.byteLength - offset);
		const messageLength = view.getUint32(1, false);
		const frameEnd = offset + 5 + messageLength;
		if (frameEnd > payload.length || (flags & 0b0000_0001) !== 0) return null;
		if ((flags & 0b0000_0010) === 0) return payload.subarray(offset + 5, frameEnd);
		offset = frameEnd;
	}
	return null;
}

function decodeUnary<T extends ProtoMessage>(codec: MessageCodec<T>, payload: Uint8Array): T | null {
	if (payload.length === 0) return null;
	const framed = decodeConnectUnaryBody(payload);
	try {
		return fromBinary(codec, framed ?? payload);
	} catch {
		return null;
	}
}

function createCursorReferenceMap(): Map<string, ModelSpec<"cursor-agent">> {
	const references = new Map<string, ModelSpec<"cursor-agent">>();
	for (const model of getBundledModels("cursor")) {
		references.set(model.id, toModelSpec(model as Model<"cursor-agent">));
	}
	return references;
}

function localEffortLevel(value: string | undefined): Effort | "off" | undefined {
	if (value === "off") return "off";
	return THINKING_EFFORTS.find(effort => effort === value);
}

function routedEffortSuffix(
	modelId: string,
): { readonly base: string; readonly level: Effort | "off"; readonly fast: boolean } | undefined {
	const route = cursorModelRoute(modelId);
	const routedLevel = localEffortLevel(route?.parameters.find(parameter => parameter.id === "effort")?.value);
	if (route === undefined || routedLevel === undefined) return undefined;
	const fast = route.parameters.some(parameter => parameter.id === "fast" && parameter.value === "true");
	const candidate = fast && modelId.endsWith("-fast") ? modelId.slice(0, -"-fast".length) : modelId;
	for (const tier of cursorEffortPreference()) {
		if (localEffortLevel(cursorEffortLevel(tier)) !== routedLevel || !candidate.endsWith(`-${tier}`)) continue;
		return { base: candidate.slice(0, -tier.length - 1), level: routedLevel, fast };
	}
	return undefined;
}

function familyFor(
	model: ModelDetails,
	availableModels: readonly AvailableModelsResponse_AvailableModel[],
): { readonly id: string; readonly level: Effort | "off" } {
	const generic = cursorEffortTierSuffix(model.modelId);
	const genericLevel = localEffortLevel(generic?.level);
	const matched =
		generic !== undefined && genericLevel !== undefined
			? { base: generic.base, level: genericLevel, fast: generic.fast }
			: routedEffortSuffix(model.modelId);
	if (matched === undefined) return { id: model.modelId, level: "off" };
	const id = `${matched.base}${matched.fast ? "-fast" : ""}`;
	const described = availableModels.some(
		available =>
			available.name === id ||
			available.idAliases.includes(id) ||
			available.legacySlugs.includes(model.modelId) ||
			available.variants.some(variant => variant.legacySlug === model.modelId),
	);
	return described ? { id, level: matched.level } : { id: model.modelId, level: "off" };
}

function modelFamilies(
	models: readonly ModelDetails[],
	availableModels: readonly AvailableModelsResponse_AvailableModel[],
): ModelFamily[] {
	const grouped = new Map<string, { model: ModelDetails; level: Effort | "off" }[]>();
	for (const model of models) {
		if (model.modelId === "") continue;
		const member = familyFor(model, availableModels);
		const group = grouped.get(member.id) ?? [];
		group.push({ model, level: member.level });
		grouped.set(member.id, group);
	}
	return [...grouped].map(([id, members]) => ({ id, members }));
}

function baseModelFor(
	family: ModelFamily,
	models: readonly AvailableModelsResponse_AvailableModel[],
): AvailableModelsResponse_AvailableModel | undefined {
	const memberIds = new Set(family.members.map(({ model }) => model.modelId));
	return models.find(
		model =>
			model.name === family.id ||
			model.idAliases.includes(family.id) ||
			model.legacySlugs.some(slug => memberIds.has(slug)) ||
			model.variants.some(variant => variant.legacySlug !== undefined && memberIds.has(variant.legacySlug)),
	);
}

function tooltipText(tooltip: AvailableModelsResponse_AvailableModel["tooltipData"]): string {
	return tooltip === undefined
		? ""
		: [tooltip.primaryText, tooltip.secondaryText, tooltip.tertiaryText, tooltip.markdownContent ?? ""].join("\n");
}

function hasDistinctMaxMode(model: AvailableModelsResponse_AvailableModel): boolean {
	if (model.supportsMaxMode !== true) return false;
	if (model.supportsNonMaxMode === false) return true;
	if (
		model.contextTokenLimitForMaxMode !== undefined &&
		model.contextTokenLimitForMaxMode !== model.contextTokenLimit
	) {
		return true;
	}
	if (tooltipText(model.tooltipDataForMaxMode) !== tooltipText(model.tooltipData)) return true;
	if (model.variants.some(variant => variant.isMaxMode)) return true;
	const normal = model.variants.find(variant => variant.isDefaultNonMaxConfig === true);
	const max = model.variants.find(variant => variant.isDefaultMaxConfig === true);
	return normal !== undefined && max !== undefined && normal !== max;
}

function variantContext(model: AvailableModelsResponse_AvailableModel, maxMode: boolean): string | undefined {
	const variant = model.variants.find(candidate =>
		maxMode ? candidate.isDefaultMaxConfig === true : candidate.isDefaultNonMaxConfig === true,
	);
	return variant?.parameterValues.find(parameter => parameter.id === "context")?.value;
}

function contextParameterTokens(value: string | undefined): number | undefined {
	const match = /^(\d+(?:\.\d+)?)([km])$/u.exec(value ?? "");
	if (match === null) return undefined;
	const amount = Number(match[1]);
	const tokens = amount * (match[2] === "m" ? 1_000_000 : 1_000);
	return Number.isSafeInteger(tokens) && tokens > 0 ? tokens : undefined;
}

function contextWindow(model: AvailableModelsResponse_AvailableModel, maxMode: boolean): number {
	const selected = contextParameterTokens(variantContext(model, maxMode));
	if (selected !== undefined) return selected;
	const captured = maxMode ? (model.contextTokenLimitForMaxMode ?? model.contextTokenLimit) : model.contextTokenLimit;
	return captured !== undefined && captured > 0 ? captured : DEFAULT_CONTEXT_WINDOW;
}

function displayName(model: ModelDetails): string {
	return model.displayName || model.displayNameShort || model.displayModelId || model.modelId;
}

function stripEffortDisplayLabel(name: string): string {
	for (const label of cursorEffortDisplayLabels()) {
		const beforeFast = ` ${label} Fast`;
		if (name.endsWith(beforeFast)) return `${name.slice(0, -beforeFast.length)} Fast`;
		const suffix = ` ${label}`;
		if (name.endsWith(suffix)) return name.slice(0, -suffix.length);
	}
	return name;
}

function familyReference(
	family: ModelFamily,
	references: ReadonlyMap<string, ModelSpec<"cursor-agent">>,
): ModelSpec<"cursor-agent"> | undefined {
	return references.get(family.id) ?? family.members.flatMap(({ model }) => references.get(model.modelId) ?? [])[0];
}

function maxModeModelId(familyId: string): string {
	return `${familyId}-1m`;
}

function providerModel(
	family: ModelFamily,
	baseUrl: string,
	base: AvailableModelsResponse_AvailableModel,
	maxMode: boolean,
	references: ReadonlyMap<string, ModelSpec<"cursor-agent">>,
): ModelSpec<"cursor-agent"> {
	const representative =
		cursorEffortPreference().flatMap(tier => {
			const level = localEffortLevel(cursorEffortLevel(tier));
			return level === undefined ? [] : family.members.filter(member => member.level === level);
		})[0] ?? family.members[0];
	if (representative === undefined) throw new Error(`Cursor model family '${family.id}' is empty`);
	const effortRouting: Partial<Record<Effort | "off", string>> = {};
	for (const member of family.members) effortRouting[member.level] = member.model.modelId;
	const efforts = THINKING_EFFORTS.filter(level => effortRouting[level] !== undefined);
	const reasoning = base.supportsThinking === true;
	const reference = familyReference(family, references);
	const capturedName =
		base.clientDisplayName === undefined || base.clientDisplayName === ""
			? stripEffortDisplayLabel(displayName(representative.model)).trim()
			: base.clientDisplayName;
	const cursorContext = variantContext(base, maxMode);
	return {
		...(reference ?? {
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			maxTokens: DEFAULT_MAX_TOKENS,
		}),
		id: maxMode ? maxModeModelId(family.id) : family.id,
		name: `${capturedName}${maxMode ? " Max" : ""}`,
		provider: "cursor",
		api: "cursor-agent",
		baseUrl,
		reasoning,
		input: base.supportsImages === true ? ["text", "image"] : ["text"],
		contextWindow: contextWindow(base, maxMode),
		maxTokens: reference?.maxTokens ?? DEFAULT_MAX_TOKENS,
		cursorMaxMode: maxMode,
		...(cursorContext === undefined ? {} : { cursorContext }),
		requestModelId: representative.model.modelId,
		...(reasoning && efforts.length > 0
			? {
					thinking: {
						mode: "effort" as const,
						efforts,
						effortRouting,
						requiresEffort: effortRouting.off === undefined,
					},
				}
			: { thinking: undefined }),
	};
}

/** Joins selectable families to authoritative capability, context, variant, and Max Mode metadata. */
export function cursorCatalogModels(
	available: AvailableModelsResponse,
	usable: GetUsableModelsResponse,
	defaultModel: GetDefaultModelForCliResponse,
	baseUrl: string,
	references: ReadonlyMap<string, ModelSpec<"cursor-agent">> = createCursorReferenceMap(),
): ModelSpec<"cursor-agent">[] {
	if (available.models.length === 0) throw new Error("Cursor AvailableModels returned no models");
	const models = modelFamilies(usable.models, available.models).flatMap(family => {
		const base = baseModelFor(family, available.models);
		if (base === undefined) return [];
		return [
			...(base.supportsNonMaxMode === false ? [] : [providerModel(family, baseUrl, base, false, references)]),
			...(hasDistinctMaxMode(base) ? [providerModel(family, baseUrl, base, true, references)] : []),
		];
	});
	if (models.length === 0) throw new Error("Cursor catalog returned no fully described usable models");
	const selected = defaultModel.model;
	if (selected !== undefined && selected.modelId !== "") {
		const usableIds = new Set(usable.models.map(model => model.modelId));
		if (!usableIds.has(selected.modelId)) throw new Error(`Cursor default model '${selected.modelId}' is not usable`);
		const family = familyFor(selected, available.models).id;
		if (!models.some(model => model.id === family || model.id === maxModeModelId(family))) {
			throw new Error(`Cursor default model '${selected.modelId}' has no complete catalog metadata`);
		}
	}
	return models;
}

/**
 * Fetches and joins `AvailableModels`, `GetUsableModels`, and `GetDefaultModelForCli`.
 * Returns `null` on request/decode/join failures and never falls back to stale partial metadata.
 */
export async function fetchCursorUsableModels(
	options: CursorModelDiscoveryOptions,
): Promise<ModelSpec<"cursor-agent">[] | null> {
	const timeoutMs = options.timeoutMs ?? 10_000;
	try {
		const baseUrl = (options.baseUrl ?? CURSOR_DEFAULT_BASE_URL).replace(/\/+$/, "");
		const availableRequest = create(AvailableModelsRequestSchema, {
			useModelParameters: true,
			doNotUseMarkdown: true,
		});
		const usableRequest = create(GetUsableModelsRequestSchema, {
			customModelIds: normalizeCustomModelIds(options.customModelIds),
		});
		const defaultRequest = create(GetDefaultModelForCliRequestSchema);
		const [availableBytes, usableBytes, defaultBytes] = await Promise.all([
			fetchViaHttp2(
				baseUrl,
				CURSOR_AVAILABLE_MODELS_PATH,
				toBinary(AvailableModelsRequestSchema, availableRequest),
				options,
				timeoutMs,
			),
			fetchViaHttp2(
				baseUrl,
				CURSOR_GET_USABLE_MODELS_PATH,
				toBinary(GetUsableModelsRequestSchema, usableRequest),
				options,
				timeoutMs,
			),
			fetchViaHttp2(
				baseUrl,
				CURSOR_GET_DEFAULT_MODEL_PATH,
				toBinary(GetDefaultModelForCliRequestSchema, defaultRequest),
				options,
				timeoutMs,
			),
		]);
		if (availableBytes === null || usableBytes === null || defaultBytes === null) return null;
		const available = decodeUnary(AvailableModelsResponseSchema, availableBytes);
		const usable = decodeUnary(GetUsableModelsResponseSchema, usableBytes);
		const defaultModel = decodeUnary(GetDefaultModelForCliResponseSchema, defaultBytes);
		if (available === null || usable === null || defaultModel === null) return null;
		return cursorCatalogModels(available, usable, defaultModel, baseUrl);
	} catch {
		return null;
	}
}

/**
 * Resolves input modalities from a bundled reference when available. Without a
 * reference, known multimodal model classes retain image support.
 */
export function resolveCursorInput(id: string, referenceInput?: ("text" | "image")[]): ("text" | "image")[] {
	if (referenceInput) return referenceInput;
	const identity = classifyModel("cursor", id, { lenient: true });
	if (identity.class === "anthropic" || identity.class === "gemini" || identity.class === "openai") {
		return ["text", "image"];
	}
	return ["text"];
}
