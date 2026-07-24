import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "bun:test";
import * as path from "node:path";
import { Agent, type StreamFn } from "@oh-my-pi/pi-agent-core";
import type { Model } from "@oh-my-pi/pi-ai";
import { createMockModel } from "@oh-my-pi/pi-ai/providers/mock";
import { getBundledModel } from "@oh-my-pi/pi-catalog/models";
import { ModelRegistry } from "@oh-my-pi/pi-coding-agent/config/model-registry";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { ExtensionRuntime, loadExtensionFromFactory } from "@oh-my-pi/pi-coding-agent/extensibility/extensions/loader";
import type { ExtensionRunner } from "@oh-my-pi/pi-coding-agent/extensibility/extensions/runner";
import { AgentSession } from "@oh-my-pi/pi-coding-agent/session/agent-session";
import { AuthStorage } from "@oh-my-pi/pi-coding-agent/session/auth-storage";
import { SessionManager } from "@oh-my-pi/pi-coding-agent/session/session-manager";
import { EventBus } from "@oh-my-pi/pi-coding-agent/utils/event-bus";
import { TempDir } from "@oh-my-pi/pi-utils";

function requiredModel(provider = "anthropic"): Model {
	const model = getBundledModel("anthropic", "claude-sonnet-4-5");
	if (!model) throw new Error("Expected bundled Anthropic model");
	return provider === model.provider ? model : { ...model, provider, id: `${provider}-test` };
}

async function scopedExtensionRunner(providerId: string, cwd: string): Promise<ExtensionRunner> {
	const extension = await loadExtensionFromFactory(
		pi =>
			pi.registerUsageProvider({
				id: providerId,
				fetchUsage: async () => ({ provider: providerId, fetchedAt: 1, limits: [] }),
			}),
		cwd,
		new EventBus(),
		new ExtensionRuntime(),
		`/extensions/${providerId}.ts`,
	);
	return {
		getExtensions: () => [extension],
		hasHandlers: () => false,
		emit: async () => undefined,
		emitBeforeAgentStart: async () => undefined,
	} as unknown as ExtensionRunner;
}

function createAgent(model: Model, streamFn?: StreamFn): Agent {
	return new Agent({
		getApiKey: () => "test-key",
		initialState: { model, systemPrompt: ["Test"], tools: [], messages: [] },
		streamFn,
	});
}

function awaitAssistantMessageEnd(agent: Agent): Promise<void> {
	const handled = Promise.withResolvers<void>();
	const subscribe = agent.subscribe.bind(agent);
	agent.subscribe = listener =>
		subscribe(event => {
			const result = listener(event);
			if (event.type === "message_end" && event.message.role === "assistant") {
				Promise.resolve(result).then(handled.resolve, handled.reject);
			}
			return result;
		});
	return handled.promise;
}

describe("AgentSession extension usage-provider attribution", () => {
	let tempDir: TempDir;
	let authStorage: AuthStorage;
	let modelRegistry: ModelRegistry;
	let session: AgentSession | undefined;

	beforeAll(async () => {
		tempDir = TempDir.createSync("@pi-session-usage-provider-attribution-");
		authStorage = await AuthStorage.create(path.join(tempDir.path(), "auth.db"));
		modelRegistry = new ModelRegistry(authStorage);
	});

	afterEach(async () => {
		await session?.dispose();
		session = undefined;
		vi.restoreAllMocks();
	});

	afterAll(() => {
		authStorage.close();
		tempDir.removeSync();
	});

	it("attributes provider usage headers to the registered extension scope", async () => {
		const model = requiredModel();
		const agent = createAgent(model);
		let responseInterceptor:
			| ((response: { headers: Headers }, responseModel?: Model) => Promise<void> | void)
			| undefined;
		const setProviderResponseInterceptor = agent.setProviderResponseInterceptor.bind(agent);
		agent.setProviderResponseInterceptor = interceptor => {
			responseInterceptor = interceptor as typeof responseInterceptor;
			setProviderResponseInterceptor(interceptor);
		};
		session = new AgentSession({
			agent,
			sessionManager: SessionManager.inMemory(),
			settings: Settings.isolated({ "compaction.enabled": false }),
			modelRegistry,
			extensionRunner: await scopedExtensionRunner(model.provider, tempDir.path()),
		});
		const ingestUsageHeaders = vi.spyOn(authStorage, "ingestUsageHeaders");

		await responseInterceptor?.({ headers: new Headers({ "x-ratelimit-remaining": "9" }) }, model);

		expect(ingestUsageHeaders).toHaveBeenCalledWith(model.provider, expect.any(Headers), {
			sessionId: session.sessionId,
			usageScopeId: session.usageProviderScopeId,
			baseUrl: modelRegistry.getProviderBaseUrl?.(model.provider),
		});
	});

	it("attributes usage-limit recovery to the registered extension scope", async () => {
		const model = requiredModel();
		const mock = createMockModel({
			provider: model.provider,
			id: model.id,
			responses: [{ throw: "429 usage_limit_reached" }],
		});
		authStorage.setRuntimeApiKey(model.provider, "test-key");
		const agent = createAgent(model, mock.stream);
		const assistantMessageEnd = awaitAssistantMessageEnd(agent);
		session = new AgentSession({
			agent,
			sessionManager: SessionManager.inMemory(),
			settings: Settings.isolated({ "compaction.enabled": false, "retry.baseDelayMs": 0, "retry.maxRetries": 1 }),
			modelRegistry,
			extensionRunner: await scopedExtensionRunner(model.provider, tempDir.path()),
		});
		const markUsageLimitReached = vi
			.spyOn(authStorage, "markUsageLimitReached")
			.mockResolvedValue({ switched: false });

		await session.prompt("Trigger scoped quota recovery");
		await assistantMessageEnd;

		expect(markUsageLimitReached).toHaveBeenCalledWith(model.provider, session.sessionId, {
			usageScopeId: session.usageProviderScopeId,
			retryAfterMs: expect.any(Number),
			baseUrl: model.baseUrl,
			modelId: model.id,
		});
	});

	it("attributes opencode-go cost recording to the registered extension scope", async () => {
		const model = requiredModel("opencode-go");
		const mock = createMockModel({
			provider: model.provider,
			id: model.id,
			responses: [
				{
					content: ["Recorded"],
					usage: { input: 1, output: 1, totalTokens: 2, cost: { total: 0.42 } },
				},
			],
		});
		authStorage.setRuntimeApiKey(model.provider, "test-key");
		const agent = createAgent(model, mock.stream);
		const assistantMessageEnd = awaitAssistantMessageEnd(agent);
		session = new AgentSession({
			agent,
			sessionManager: SessionManager.inMemory(),
			settings: Settings.isolated({ "compaction.enabled": false }),
			modelRegistry,
			extensionRunner: await scopedExtensionRunner(model.provider, tempDir.path()),
		});
		const recordUsageCost = vi.spyOn(authStorage, "recordUsageCost");

		await session.prompt("Record scoped cost");
		await assistantMessageEnd;
		expect(session.messages.at(-1)).toMatchObject({ provider: "opencode-go" });

		expect(recordUsageCost).toHaveBeenCalledWith("opencode-go", 0.42, {
			sessionId: session.sessionId,
			usageScopeId: session.usageProviderScopeId,
			recordedAt: expect.any(Number),
			baseUrl: modelRegistry.getProviderBaseUrl?.("opencode-go"),
		});
	});
});
