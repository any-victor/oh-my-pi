import { afterEach, describe, expect, test, vi } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import * as ai from "@oh-my-pi/pi-ai";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { mnemopiBackend } from "@oh-my-pi/pi-coding-agent/mnemopi/backend";
import { getMnemopiSessionState } from "@oh-my-pi/pi-coding-agent/mnemopi/state";

describe("mnemopi provider credentials", () => {
	let agentDir: string | undefined;

	afterEach(async () => {
		vi.restoreAllMocks();
		if (agentDir) await fs.rm(agentDir, { force: true, recursive: true });
		agentDir = undefined;
	});

	test("resolves OpenRouter credentials in the AgentSession usage-provider scope", async () => {
		agentDir = await fs.mkdtemp(path.join(os.tmpdir(), "mnemopi-provider-scope-"));
		const settings = Settings.isolated({
			"mnemopi.autoRecall": false,
			"mnemopi.autoRetain": false,
			"mnemopi.dbPath": path.join(agentDir, "mnemopi.db"),
			"mnemopi.noEmbeddings": true,
			"mnemopi.scoping": "global",
		});
		const resolver = vi.fn(() => async () => "openrouter-key");
		const modelRegistry = {
			getApiKeyForProvider: vi.fn(async () => "openrouter-key"),
			getAvailable: vi.fn(() => []),
			resolver,
		};
		const session = {
			sessionId: "provider-session",
			usageProviderScopeId: "usage-provider-scope",
			settings,
			modelRegistry,
			sessionManager: { getCwd: () => agentDir },
			subscribe: vi.fn(() => () => {}),
		};

		await mnemopiBackend.start({ agentDir, modelRegistry, session, settings, taskDepth: 0 } as never);

		expect(modelRegistry.getApiKeyForProvider).toHaveBeenCalledWith("openrouter", "provider-session", {
			usageScopeId: "usage-provider-scope",
		});
		expect(resolver).toHaveBeenCalledWith("openrouter", {
			sessionId: "provider-session",
			usageScopeId: "usage-provider-scope",
		});
		await mnemopiBackend.clear(agentDir, agentDir, session as never);
	});

	test("resolves smol LLM credentials in the AgentSession usage-provider scope", async () => {
		agentDir = await fs.mkdtemp(path.join(os.tmpdir(), "mnemopi-provider-scope-"));
		const settings = Settings.isolated({
			"mnemopi.autoRecall": false,
			"mnemopi.autoRetain": false,
			"mnemopi.dbPath": path.join(agentDir, "mnemopi.db"),
			"mnemopi.noEmbeddings": true,
			"mnemopi.scoping": "global",
			modelRoles: { tiny: "openai/smol-model" },
		});
		const model = {
			contextWindow: 32_000,
			id: "smol-model",
			name: "Smol model",
			provider: "openai",
		};
		const resolver = vi.fn(() => async () => "api-key");
		const modelRegistry = {
			getApiKey: vi.fn(async () => "api-key"),
			getApiKeyForProvider: vi.fn(async () => "openrouter-key"),
			getAvailable: vi.fn(() => [model]),
			resolver,
		};
		const session = {
			sessionId: "provider-session",
			usageProviderScopeId: "usage-provider-scope",
			settings,
			modelRegistry,
			sessionManager: { getCwd: () => agentDir },
			subscribe: vi.fn(() => () => {}),
		};
		vi.spyOn(ai, "completeSimple").mockResolvedValue({
			content: [{ text: "completion", type: "text" }],
		} as never);

		await mnemopiBackend.start({ agentDir, modelRegistry, session, settings, taskDepth: 0 } as never);
		const llm = getMnemopiSessionState(session as never)?.config.providerOptions.llm;
		if (typeof llm !== "function") throw new Error("Expected the smol LLM closure");
		await llm("test prompt");

		expect(modelRegistry.getApiKey).toHaveBeenCalledWith(model, "provider-session", "usage-provider-scope");
		expect(resolver).toHaveBeenCalledWith(model, {
			sessionId: "provider-session",
			usageScopeId: "usage-provider-scope",
		});
		await mnemopiBackend.clear(agentDir, agentDir, session as never);
	});
});
