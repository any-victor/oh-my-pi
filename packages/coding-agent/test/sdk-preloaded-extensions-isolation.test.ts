/**
 * Regression guard for issue #2190 / PR #2193 review.
 *
 * The CLI loads extensions early to parse custom flags, then hands the result
 * back through `preloadedExtensions` so its OWN session can reuse the loaded
 * instances without redoing the FS scan. `createAgentSession()` augments the
 * result with inline extensions (autoresearch + custom-tools wrapper), so it
 * MUST clone the caller's `extensions` array before mutating it — otherwise
 * the caller's array accumulates session-local wrappers it never authored.
 *
 * Subagent forwarding is a separate path (`preloadedExtensionPaths`) which
 * reloads extensions per session so each session's `ExtensionAPI` is its own.
 */
import { afterAll, beforeAll, describe, expect, it, spyOn } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { UsageProvider } from "@oh-my-pi/pi-ai";
import { getBundledModel } from "@oh-my-pi/pi-catalog/models";
import { ModelRegistry } from "@oh-my-pi/pi-coding-agent/config/model-registry";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { ExtensionRuntime, loadExtensionFromFactory } from "@oh-my-pi/pi-coding-agent/extensibility/extensions/loader";
import type { LoadExtensionsResult } from "@oh-my-pi/pi-coding-agent/extensibility/extensions/types";
import { createAgentSession } from "@oh-my-pi/pi-coding-agent/sdk";
import { AuthStorage } from "@oh-my-pi/pi-coding-agent/session/auth-storage";
import { SessionManager } from "@oh-my-pi/pi-coding-agent/session/session-manager";
import { EventBus } from "@oh-my-pi/pi-coding-agent/utils/event-bus";
import { removeSyncWithRetries } from "@oh-my-pi/pi-utils";

describe("createAgentSession preloadedExtensions isolation (issue #2190)", () => {
	let sharedDir: string;
	let authStorage: AuthStorage;
	let modelRegistry: ModelRegistry;

	beforeAll(async () => {
		sharedDir = fs.mkdtempSync(path.join(os.tmpdir(), "pi-preloaded-ext-"));
		authStorage = await AuthStorage.create(path.join(sharedDir, "auth.db"));
		modelRegistry = new ModelRegistry(authStorage, path.join(sharedDir, "models.yml"));
	});

	afterAll(() => {
		authStorage.close();
		removeSyncWithRetries(sharedDir);
	});

	it("does not mutate the caller's extensions array when preloadedExtensions is provided", async () => {
		const preloaded: LoadExtensionsResult = {
			extensions: [],
			errors: [],
			runtime: {
				flagValues: new Map(),
				pendingProviderRegistrations: [],
				// Cast: only the fields we touch matter; the SDK happily accepts a
				// minimal runtime when no extension hooks fire.
			} as unknown as LoadExtensionsResult["runtime"],
		};
		const beforeLength = preloaded.extensions.length;
		const beforeArrayRef = preloaded.extensions;

		const session = await createAgentSession({
			cwd: sharedDir,
			agentDir: sharedDir,
			sessionManager: SessionManager.inMemory(),
			modelRegistry,
			settings: Settings.isolated(),
			preloadedExtensions: preloaded,
			// Disable everything that would touch the network / FS scans.
			enableLsp: false,
			enableMCP: false,
			skipPythonPreflight: true,
			skills: [],
			rules: [],
			preloadedCustomToolPaths: [],
			contextFiles: [],
			promptTemplates: [],
		});
		await session.session.dispose();

		// The session's own `extensionsResult` carries inline wrappers, but the
		// caller's array (and its identity) must be untouched.
		expect(preloaded.extensions).toBe(beforeArrayRef);
		expect(preloaded.extensions.length).toBe(beforeLength);
	});

	it("replays source-owned usage providers when a preloaded result is reused", async () => {
		const provider: UsageProvider = {
			id: "reusable-usage",
			fetchUsage: async () => ({ provider: "reusable-usage", fetchedAt: 1, limits: [] }),
		};
		await authStorage.set(provider.id, { type: "api_key", key: "reusable-key" });
		const runtime = new ExtensionRuntime();
		const extension = await loadExtensionFromFactory(
			pi => pi.registerUsageProvider(provider),
			sharedDir,
			new EventBus(),
			runtime,
			"/extensions/reusable.ts",
		);
		const preloaded: LoadExtensionsResult = { extensions: [extension], errors: [], runtime };

		for (let index = 0; index < 2; index++) {
			const session = await createAgentSession({
				cwd: sharedDir,
				agentDir: sharedDir,
				sessionManager: SessionManager.inMemory(),
				modelRegistry,
				settings: Settings.isolated(),
				preloadedExtensions: preloaded,
				enableLsp: false,
				enableMCP: false,
				skipPythonPreflight: true,
				skills: [],
				rules: [],
				preloadedCustomToolPaths: [],
				contextFiles: [],
				promptTemplates: [],
			});
			expect((await session.session.fetchUsageReports())?.map(report => report.provider)).toContain(provider.id);
			await session.session.dispose();
		}
		expect(authStorage.usageProviderFor(provider.id)).toBeUndefined();
	});

	it("partitions inline usage-provider cache entries by session", async () => {
		const providerId = "inline-cache-isolated";
		await authStorage.set(providerId, { type: "api_key", key: "inline-cache-key" });
		let firstFetches = 0;
		let secondFetches = 0;
		const first = await createAgentSession({
			cwd: sharedDir,
			agentDir: sharedDir,
			sessionManager: SessionManager.inMemory(),
			modelRegistry,
			settings: Settings.isolated(),
			disableExtensionDiscovery: true,
			extensions: [
				pi =>
					pi.registerUsageProvider({
						id: providerId,
						fetchUsage: async () => {
							firstFetches++;
							return { provider: providerId, fetchedAt: 1, limits: [] };
						},
					}),
			],
			enableLsp: false,
			enableMCP: false,
			skipPythonPreflight: true,
			skills: [],
			rules: [],
			contextFiles: [],
			promptTemplates: [],
		});
		try {
			expect(await first.session.fetchUsageReports()).toEqual([{ provider: providerId, fetchedAt: 1, limits: [] }]);
			const second = await createAgentSession({
				cwd: sharedDir,
				agentDir: sharedDir,
				sessionManager: SessionManager.inMemory(),
				modelRegistry,
				settings: Settings.isolated(),
				disableExtensionDiscovery: true,
				extensions: [
					pi =>
						pi.registerUsageProvider({
							id: providerId,
							fetchUsage: async () => {
								secondFetches++;
								return { provider: providerId, fetchedAt: 2, limits: [] };
							},
						}),
				],
				enableLsp: false,
				enableMCP: false,
				skipPythonPreflight: true,
				skills: [],
				rules: [],
				contextFiles: [],
				promptTemplates: [],
			});
			try {
				expect(await second.session.fetchUsageReports()).toEqual([
					{ provider: providerId, fetchedAt: 2, limits: [] },
				]);
				expect(firstFetches).toBe(1);
				expect(secondFetches).toBe(1);
			} finally {
				await second.session.dispose();
			}
		} finally {
			await first.session.dispose();
		}
	});

	it("isolates source-owned usage providers between sessions sharing AuthStorage", async () => {
		const defaultModel = getBundledModel("anthropic", "claude-sonnet-4-5");
		if (!defaultModel) throw new Error("expected bundled anthropic default model");
		authStorage.setRuntimeApiKey(defaultModel.provider, "test-key");
		const parentSettings = Settings.isolated();
		parentSettings.setModelRole("default", `${defaultModel.provider}/${defaultModel.id}`);
		const provider: UsageProvider = {
			id: "parent-owned-usage",
			fetchUsage: async () => ({ provider: "parent-owned-usage", fetchedAt: 1, limits: [] }),
		};
		await authStorage.set(provider.id, { type: "api_key", key: "parent-key" });
		const runtime = new ExtensionRuntime();
		const extension = await loadExtensionFromFactory(
			pi => pi.registerUsageProvider(provider),
			sharedDir,
			new EventBus(),
			runtime,
			"/extensions/parent.ts",
		);
		const parent = await createAgentSession({
			cwd: sharedDir,
			agentDir: sharedDir,
			sessionManager: SessionManager.inMemory(),
			modelRegistry,
			settings: parentSettings,
			providerSessionId: "shared-provider-session",
			preloadedExtensions: { extensions: [extension], errors: [], runtime },
			enableLsp: false,
			enableMCP: false,
			skipPythonPreflight: true,
			skills: [],
			rules: [],
			preloadedCustomToolPaths: [],
			contextFiles: [],
			promptTemplates: [],
		});
		try {
			const child = await createAgentSession({
				cwd: sharedDir,
				agentDir: sharedDir,
				sessionManager: SessionManager.inMemory(),
				modelRegistry,
				settings: Settings.isolated(),
				providerSessionId: "shared-provider-session",
				preloadedExtensions: { extensions: [], errors: [], runtime: new ExtensionRuntime() },
				enableLsp: false,
				enableMCP: false,
				skipPythonPreflight: true,
				skills: [],
				rules: [],
				preloadedCustomToolPaths: [],
				contextFiles: [],
				promptTemplates: [],
			});
			try {
				expect(parent.session.usageProviderScopeId).not.toBe(child.session.usageProviderScopeId);
				expect((await parent.session.fetchUsageReports())?.map(report => report.provider)).toContain(provider.id);
				expect((await child.session.fetchUsageReports())?.map(report => report.provider)).not.toContain(
					provider.id,
				);
				expect((await parent.session.fetchUsageReports())?.map(report => report.provider)).toContain(provider.id);
				expect(authStorage.usageProviderFor(provider.id)).toBeUndefined();
				const model = parent.session.model;
				if (!model) throw new Error("expected parent model");
				expect(model.provider).toBe(defaultModel.provider);
				const resolverSpy = spyOn(modelRegistry, "resolver").mockImplementation(
					(_requestModel, options) => async () =>
						typeof options !== "string" && options?.usageScopeId === parent.session.usageProviderScopeId
							? "scoped-key"
							: undefined,
				);
				try {
					const key = await parent.session.agent.getApiKey?.(model);
					if (typeof key !== "function") throw new Error("expected API key resolver");
					expect(await key({ lastChance: false, error: undefined })).toBe("scoped-key");
				} finally {
					resolverSpy.mockRestore();
				}
			} finally {
				await child.session.dispose();
			}
		} finally {
			await parent.session.dispose();
		}
	});
});
