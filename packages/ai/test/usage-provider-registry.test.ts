import { describe, expect, it, spyOn } from "bun:test";
import { type AuthCredentialStore, AuthStorage, type StoredAuthCredential } from "@oh-my-pi/pi-ai/auth-storage";
import type { Provider } from "@oh-my-pi/pi-ai/types";
import {
	type CredentialRankingStrategy,
	type UsageProvider,
	UsageProviderRegistry,
	type UsageReport,
} from "@oh-my-pi/pi-ai/usage";

function createUsageProvider(id: Provider): UsageProvider {
	return {
		id,
		async fetchUsage() {
			return null;
		},
	};
}

function createStore(rows: StoredAuthCredential[] = []): AuthCredentialStore {
	const cache = new Map<string, { value: string; expiresAtSec: number }>();
	return {
		close() {},
		listAuthCredentials(provider) {
			return provider === undefined ? rows : rows.filter(row => row.provider === provider);
		},
		updateAuthCredential() {},
		deleteAuthCredential() {},
		tryDisableAuthCredentialIfMatches() {
			return false;
		},
		replaceAuthCredentialsForProvider() {
			return rows;
		},
		upsertAuthCredentialForProvider() {
			return rows;
		},
		deleteAuthCredentialsForProvider() {},
		getCache(key, options) {
			const entry = cache.get(key);
			if (!entry || (!options?.includeExpired && entry.expiresAtSec * 1_000 <= Date.now())) return null;
			return entry.value;
		},
		setCache(key, value, expiresAtSec) {
			cache.set(key, { value, expiresAtSec });
		},
		cleanExpiredCache() {},
	};
}

describe("UsageProviderRegistry", () => {
	it("uses the most recently registered source for a provider", () => {
		const registry = new UsageProviderRegistry();
		const first = createUsageProvider("anthropic");
		const latest = createUsageProvider("anthropic");

		registry.register(first, "extension:first");
		registry.register(latest, "extension:latest");

		expect(registry.resolve("anthropic")).toBe(latest);
	});

	it("replaces and promotes a registration from the same source", () => {
		const registry = new UsageProviderRegistry();
		const first = createUsageProvider("anthropic");
		const otherSource = createUsageProvider("anthropic");
		const replacement = createUsageProvider("anthropic");

		registry.register(first, "extension:first");
		registry.register(otherSource, "extension:other");
		registry.register(replacement, "extension:first");

		expect(registry.resolve("anthropic")).toBe(replacement);
	});

	it("restores active registrations when sources are cleared or synchronized", () => {
		const registry = new UsageProviderRegistry();
		const fallback = createUsageProvider("anthropic");
		const override = createUsageProvider("anthropic");

		registry.register(fallback, "extension:fallback");
		registry.register(override, "extension:override");
		registry.clearSource("extension:override");
		expect(registry.resolve("anthropic")).toBe(fallback);

		registry.register(override, "extension:override");
		registry.syncSources(["extension:fallback", "extension:fallback"]);
		expect(registry.resolve("anthropic")).toBe(fallback);
	});

	it("keeps registrations isolated by registry instance", () => {
		const firstRegistry = new UsageProviderRegistry();
		const secondRegistry = new UsageProviderRegistry();
		const first = createUsageProvider("anthropic");
		const second = createUsageProvider("anthropic");

		firstRegistry.register(first, "extension:first");
		secondRegistry.register(second, "extension:second");

		expect(firstRegistry.resolve("anthropic")).toBe(first);
		expect(secondRegistry.resolve("anthropic")).toBe(second);
	});
});

describe("AuthStorage usage provider registry", () => {
	it("uses its own registry, falls back to built-ins, and restores them after an override", () => {
		const storage = new AuthStorage(createStore());
		const extensionProvider = createUsageProvider("ollama");

		try {
			const builtInProvider = storage.usageProviderFor("ollama");
			expect(builtInProvider).toBeDefined();

			storage.syncExtensionUsageProviders(
				["extension:override"],
				[{ provider: extensionProvider, sourceId: "extension:override" }],
			);
			expect(storage.usageProviderFor("ollama")).toBe(extensionProvider);

			storage.syncExtensionUsageProviders([], []);
			expect(storage.usageProviderFor("ollama")).toBe(builtInProvider);
		} finally {
			storage.close();
		}
	});

	it("keeps default AuthStorage instances isolated", () => {
		const firstStorage = new AuthStorage(createStore());
		const secondStorage = new AuthStorage(createStore());
		const extensionProvider = createUsageProvider("ollama");

		try {
			firstStorage.syncExtensionUsageProviders(
				["extension:first"],
				[{ provider: extensionProvider, sourceId: "extension:first" }],
			);
			expect(firstStorage.usageProviderFor("ollama")).toBe(extensionProvider);
			expect(secondStorage.usageProviderFor("ollama")).not.toBe(extensionProvider);
		} finally {
			firstStorage.close();
			secondStorage.close();
		}
	});

	it("rejects the whole registration batch without changing the active provider", () => {
		const storage = new AuthStorage(createStore());
		const current = createUsageProvider("anthropic");
		const replacement = createUsageProvider("anthropic");

		try {
			storage.syncExtensionUsageProviders(
				["extension:current"],
				[{ provider: current, sourceId: "extension:current" }],
			);

			expect(() =>
				storage.syncExtensionUsageProviders(
					["extension:next"],
					[
						{ provider: replacement, sourceId: "extension:next" },
						{
							provider: { id: "invalid", fetchUsage: null } as unknown as UsageProvider,
							sourceId: "extension:next",
						},
					],
				),
			).toThrow("Invalid extension usage-provider registration");
			expect(storage.usageProviderFor("anthropic")).toBe(current);

			expect(() =>
				storage.syncExtensionUsageProviders(
					["extension:next"],
					[
						{
							provider: {
								...replacement,
								validatesCredentials: "yes",
							} as unknown as UsageProvider,
							sourceId: "extension:next",
						},
					],
				),
			).toThrow("Invalid extension usage-provider registration");
			expect(storage.usageProviderFor("anthropic")).toBe(current);

			expect(() =>
				storage.syncExtensionUsageProviders(
					["extension:next"],
					[{ provider: replacement, sourceId: "extension:inactive" }],
				),
			).toThrow("Extension usage-provider source is not active");
			expect(storage.usageProviderFor("anthropic")).toBe(current);
		} finally {
			storage.close();
		}
	});

	it("keeps extension registrations ahead of an explicit fallback resolver", () => {
		const registry = new UsageProviderRegistry();
		const registered = createUsageProvider("anthropic");
		const explicit = createUsageProvider("anthropic");
		const storage = new AuthStorage(createStore(), {
			usageProviderRegistry: registry,
			usageProviderResolver: provider => (provider === "anthropic" ? explicit : undefined),
		});

		try {
			expect(storage.usageProviderFor("anthropic")).toBe(explicit);
			registry.register(registered, "extension:override");
			expect(storage.usageProviderFor("anthropic")).toBe(registered);
			registry.clearSource("extension:override");
			expect(storage.usageProviderFor("anthropic")).toBe(explicit);
		} finally {
			storage.close();
		}
	});
	it("does not reuse cached reports after the active registration changes", async () => {
		const providerId = "runtime-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		let firstCalls = 0;
		let secondCalls = 0;
		const report = (label: string): UsageReport => ({
			provider: providerId,
			fetchedAt: Date.now(),
			limits: [
				{
					id: `${providerId}:window`,
					label,
					scope: { provider: providerId, windowId: "window" },
					window: { id: "window", label: "Window" },
					amount: { usedFraction: 0.25, unit: "percent" },
					status: "ok",
				},
			],
		});
		const firstProvider: UsageProvider = {
			id: providerId,
			async fetchUsage() {
				firstCalls += 1;
				return report("First");
			},
		};
		const secondProvider: UsageProvider = {
			id: providerId,
			async fetchUsage() {
				secondCalls += 1;
				return report("Second");
			},
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:first"],
				[{ provider: firstProvider, sourceId: "extension:first" }],
			);
			expect((await storage.fetchUsageReports())?.[0]?.limits[0]?.label).toBe("First");
			expect((await storage.fetchUsageReports())?.[0]?.limits[0]?.label).toBe("First");
			expect(firstCalls).toBe(1);

			storage.syncExtensionUsageProviders(
				["extension:second"],
				[{ provider: secondProvider, sourceId: "extension:second" }],
			);
			expect((await storage.fetchUsageReports())?.[0]?.limits[0]?.label).toBe("Second");
			expect(secondCalls).toBe(1);
		} finally {
			storage.close();
		}
	});
	it("uses caller-scoped providers without leaking shared registrations", async () => {
		const providerId = "runtime-scoped-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		const sharedProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: 1, limits: [] }),
		};
		const localProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: 2, limits: [] }),
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:shared"],
				[{ provider: sharedProvider, sourceId: "extension:shared" }],
			);
			expect((await storage.fetchUsageReports())?.[0]?.fetchedAt).toBe(1);
			const unregisterLocal = storage.registerSessionUsageProviders("local-session", {
				resolve: provider => (provider === providerId ? localProvider : undefined),
				cacheKeyVersion: provider => (provider === providerId ? "local:1" : null),
				providerIds: () => [],
			});
			const unregisterEmpty = storage.registerSessionUsageProviders("empty-session", {
				resolve: () => undefined,
				cacheKeyVersion: () => null,
				providerIds: () => [],
			});
			try {
				expect(
					(await storage.fetchUsageReports({ usageScopeId: "local-session:side-channel" }))?.[0]?.fetchedAt,
				).toBe(2);
				expect(await storage.fetchUsageReports({ usageScopeId: "empty-session" })).toEqual([]);
			} finally {
				unregisterEmpty();
				unregisterLocal();
			}
			expect((await storage.fetchUsageReports())?.[0]?.fetchedAt).toBe(1);
		} finally {
			storage.close();
		}
	});
	it("isolates a scoped built-in fallback from a global extension cache", async () => {
		const providerId = "scoped-fallback-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		let builtInCalls = 0;
		let extensionCalls = 0;
		const builtInProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: ++builtInCalls, limits: [] }),
		};
		const extensionProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: 100 + ++extensionCalls, limits: [] }),
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]), {
			usageProviderResolver: provider => (provider === providerId ? builtInProvider : undefined),
		});

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:global"],
				[{ provider: extensionProvider, sourceId: "extension:global" }],
			);
			expect((await storage.fetchUsageReports())?.[0]?.fetchedAt).toBe(101);

			const unregister = storage.registerSessionUsageProviders("scoped-session", {
				resolve: () => undefined,
				cacheKeyVersion: () => null,
				providerIds: () => [],
			});
			try {
				expect((await storage.fetchUsageReports({ usageScopeId: "scoped-session" }))?.[0]?.fetchedAt).toBe(1);
				expect(builtInCalls).toBe(1);
				expect(extensionCalls).toBe(1);
			} finally {
				unregister();
			}
		} finally {
			storage.close();
		}
	});

	it("enumerates scope-only provider ids so env-backed extension providers are fetched", async () => {
		const providerId = "groq";
		const storage = new AuthStorage(createStore([]));
		const scopedProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: 7, limits: [] }),
		};
		const previousEnv = Bun.env.GROQ_API_KEY;
		Bun.env.GROQ_API_KEY = "groq-env-key";
		try {
			await storage.reload();
			const unregister = storage.registerSessionUsageProviders("env-session", {
				resolve: provider => (provider === providerId ? scopedProvider : undefined),
				cacheKeyVersion: provider => (provider === providerId ? "env:1" : null),
				providerIds: () => [providerId],
			});
			try {
				const scoped = await storage.fetchUsageReports({ usageScopeId: "env-session" });
				expect(scoped?.map(report => report.provider)).toContain(providerId);
			} finally {
				unregister();
			}
			// Without the scope registration, the id is not enumerated anywhere.
			const unscoped = await storage.fetchUsageReports();
			expect(unscoped?.map(report => report.provider) ?? []).not.toContain(providerId);
		} finally {
			if (previousEnv === undefined) delete Bun.env.GROQ_API_KEY;
			else Bun.env.GROQ_API_KEY = previousEnv;
			storage.close();
		}
	});
	it("expires extension-versioned cache keys on all-provider invalidation", async () => {
		const providerId = "invalidate-scoped-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		let fetches = 0;
		const scopedProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				fetches += 1;
				return { provider: providerId, fetchedAt: fetches, limits: [] };
			},
		};

		try {
			await storage.reload();
			const unregister = storage.registerSessionUsageProviders("local-session", {
				resolve: provider => (provider === providerId ? scopedProvider : undefined),
				cacheKeyVersion: () => "local:1",
				providerIds: () => [providerId],
			});
			const fetchedAt = async () =>
				(await storage.fetchUsageReports({ usageScopeId: "local-session" }))?.find(
					report => report.provider === providerId,
				)?.fetchedAt;
			try {
				expect(await fetchedAt()).toBe(1);
				// Cached: no refetch within TTL.
				expect(await fetchedAt()).toBe(1);
				await storage.invalidateUsageCache();
				// All-provider invalidation must also expire the extension-versioned key.
				expect(await fetchedAt()).toBe(2);
			} finally {
				unregister();
			}
		} finally {
			storage.close();
		}
	});

	it("expires scope-only env-backed cache keys on all-provider invalidation", async () => {
		const providerId = "groq";
		const storage = new AuthStorage(createStore([]));
		let fetches = 0;
		const scopedProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				fetches += 1;
				return { provider: providerId, fetchedAt: fetches, limits: [] };
			},
		};
		const previousEnv = Bun.env.GROQ_API_KEY;
		Bun.env.GROQ_API_KEY = "groq-env-key";
		try {
			await storage.reload();
			const unregister = storage.registerSessionUsageProviders("env-session", {
				resolve: provider => (provider === providerId ? scopedProvider : undefined),
				cacheKeyVersion: provider => (provider === providerId ? "env:1" : null),
				providerIds: () => [providerId],
			});
			const fetchedAt = async () =>
				(await storage.fetchUsageReports({ usageScopeId: "env-session" }))?.find(
					report => report.provider === providerId,
				)?.fetchedAt;
			try {
				expect(await fetchedAt()).toBe(1);
				// Cached within TTL: no refetch.
				expect(await fetchedAt()).toBe(1);
				await storage.invalidateUsageCache();
				// The provider has no stored credential; the env-backed api-key cache
				// key must still be expired by an all-provider invalidation.
				expect(await fetchedAt()).toBe(2);
			} finally {
				unregister();
			}
		} finally {
			if (previousEnv === undefined) delete Bun.env.GROQ_API_KEY;
			else Bun.env.GROQ_API_KEY = previousEnv;
			storage.close();
		}
	});

	it("merges a session-scoped provider over remote store usage", async () => {
		const providerId = "remote-scoped-usage";
		const accountId = "account-1";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId,
		};
		const store: AuthCredentialStore = {
			...createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]),
			fetchUsageReports: async () => [
				{ provider: providerId, fetchedAt: 1, limits: [], metadata: { accountId, remoteOnly: true } },
			],
		};
		const storage = new AuthStorage(store);
		const scopedProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({
				provider: providerId,
				fetchedAt: 2,
				limits: [],
				metadata: { accountId },
			}),
		};

		try {
			await storage.reload();
			const unregister = storage.registerSessionUsageProviders("local-session", {
				resolve: provider => (provider === providerId ? scopedProvider : undefined),
				cacheKeyVersion: () => "local:1",
				providerIds: () => [],
			});
			try {
				expect((await storage.fetchUsageReports())?.[0]?.fetchedAt).toBe(1);
				expect((await storage.fetchUsageReports({ usageScopeId: "local-session" }))?.[0]?.fetchedAt).toBe(2);
				expect(
					(await storage.fetchUsageReports({ usageScopeId: "local-session" }))?.[0]?.metadata?.remoteOnly,
				).toBeUndefined();
			} finally {
				unregister();
			}
		} finally {
			storage.close();
		}
	});

	it("fetches global extension providers alongside remote store usage", async () => {
		const providerId = "global-remote-usage";
		const credential = { type: "api_key" as const, key: "test-key" };
		const store: AuthCredentialStore = {
			...createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]),
			fetchUsageReports: async () => [
				{ provider: providerId, fetchedAt: 1, limits: [], metadata: { remoteOnly: true } },
			],
		};
		const storage = new AuthStorage(store);
		const extensionProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => ({ provider: providerId, fetchedAt: 2, limits: [] }),
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:global-remote"],
				[{ provider: extensionProvider, sourceId: "extension:global-remote" }],
			);
			expect(await storage.fetchUsageReports()).toEqual([{ provider: providerId, fetchedAt: 2, limits: [] }]);
		} finally {
			storage.close();
		}
	});

	it("keeps a global extension provider authoritative over a per-credential store hook", async () => {
		const providerId = "global-remote-oauth";
		const credential = {
			type: "oauth" as const,
			access: "extension-access",
			refresh: "extension-refresh",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		let storeFetches = 0;
		let extensionFetches = 0;
		const secondCredential = { ...credential, access: "extension-access-2", accountId: "account-2" };
		const store: AuthCredentialStore = {
			...createStore([
				{ id: 1, provider: providerId, credential, disabledCause: null },
				{ id: 2, provider: providerId, credential: secondCredential, disabledCause: null },
			]),
			getUsageReport: async () => {
				storeFetches += 1;
				return { provider: providerId, fetchedAt: 1, limits: [] };
			},
		};
		const strategy: CredentialRankingStrategy = {
			findWindowLimits: () => ({}),
			windowDefaults: { primaryMs: 60_000, secondaryMs: 60_000 },
		};
		const storage = new AuthStorage(store, { rankingStrategyResolver: () => strategy });
		const extensionProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				extensionFetches += 1;
				return { provider: providerId, fetchedAt: 2, limits: [] };
			},
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:global-oauth"],
				[{ provider: extensionProvider, sourceId: "extension:global-oauth" }],
			);
			expect(["extension-access", "extension-access-2"]).toContain(
				(await storage.getApiKey(providerId, "session-1")) ?? "",
			);
			expect(extensionFetches).toBe(2);
			expect(storeFetches).toBe(0);
		} finally {
			storage.close();
		}
	});

	it("rejects malformed reports from a store override", async () => {
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const store: AuthCredentialStore = {
			...createStore([{ id: 1, provider: "anthropic", credential, disabledCause: null }]),
			fetchUsageReports: async () =>
				[
					{ provider: "anthropic", fetchedAt: 1 },
					{ provider: "anthropic", fetchedAt: 3, limits: [] },
				] as UsageReport[],
		};
		const storage = new AuthStorage(store);

		try {
			await storage.reload();
			expect(await storage.fetchUsageReports()).toEqual([{ provider: "anthropic", fetchedAt: 3, limits: [] }]);
		} finally {
			storage.close();
		}
	});

	it("collects configured API keys for custom extension providers", async () => {
		const providerId = "configured-extension-usage";
		const storage = new AuthStorage(
			createStore([
				{
					id: 1,
					provider: providerId,
					credential: { type: "api_key", key: "stored-key" },
					disabledCause: null,
				},
			]),
		);
		const extensionProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async params => ({
				provider: providerId,
				fetchedAt: params.credential.type === "api_key" && params.credential.apiKey === "config-key" ? 2 : 1,
				limits: [],
			}),
		};

		try {
			await storage.reload();
			storage.setConfigApiKey(providerId, "config-key");
			storage.syncExtensionUsageProviders(
				["extension:configured"],
				[{ provider: extensionProvider, sourceId: "extension:configured" }],
			);
			expect((await storage.fetchUsageReports())?.[0]?.fetchedAt).toBe(2);
		} finally {
			storage.close();
		}
	});

	it("bounds last-good retention for persistent and session-scoped caches", async () => {
		const now = spyOn(Date, "now");
		let caseStart = 1_800_000_000_000;
		now.mockReturnValue(caseStart);

		const exerciseCache = async (usageScopeId?: string): Promise<void> => {
			caseStart += 100_000_000;
			now.mockReturnValue(caseStart);
			const providerId = usageScopeId ? "memory-stale-usage" : "persistent-stale-usage";
			const storage = new AuthStorage(
				createStore([
					{
						id: 1,
						provider: providerId,
						credential: { type: "api_key", key: "test-key" },
						disabledCause: null,
					},
				]),
			);
			let fail = false;
			let fetchCalls = 0;
			const provider: UsageProvider = {
				id: providerId,
				fetchUsage: async () => {
					fetchCalls += 1;
					return fail ? null : { provider: providerId, fetchedAt: caseStart, limits: [] };
				},
			};
			let unregister: (() => void) | undefined;

			try {
				await storage.reload();
				if (usageScopeId) {
					unregister = storage.registerSessionUsageProviders(usageScopeId, {
						resolve: candidate => (candidate === providerId ? provider : undefined),
						cacheKeyVersion: () => "test:1",
						providerIds: () => [],
					});
				} else {
					storage.syncExtensionUsageProviders(["extension:stale"], [{ provider, sourceId: "extension:stale" }]);
				}
				const options = usageScopeId ? { usageScopeId } : undefined;
				expect((await storage.fetchUsageReports(options))?.[0]?.fetchedAt).toBe(caseStart);
				expect(fetchCalls).toBe(1);
				fail = true;
				now.mockReturnValue(caseStart + 24 * 3_600_000 - 1_000);
				expect((await storage.fetchUsageReports(options))?.[0]?.fetchedAt).toBe(caseStart);
				expect(fetchCalls).toBe(2);
				now.mockReturnValue(caseStart + 24 * 3_600_000 + 1_000);
				expect(await storage.fetchUsageReports(options)).toEqual([]);
				expect(fetchCalls).toBe(3);
			} finally {
				unregister?.();
				storage.close();
			}
		};

		try {
			await exerciseCache();
			await exerciseCache("session-scope");
		} finally {
			now.mockRestore();
		}
	});

	it("coalesces concurrent reports across session scopes with the same durable cache key", async () => {
		const providerId = "concurrent-scoped-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		let firstCalls = 0;
		let secondCalls = 0;
		const { promise: firstStarted, resolve: markFirstStarted } = Promise.withResolvers<void>();
		const { promise: firstCanFinish, resolve: releaseFirst } = Promise.withResolvers<void>();
		const firstProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				firstCalls += 1;
				markFirstStarted();
				await firstCanFinish;
				return { provider: providerId, fetchedAt: 1, limits: [] };
			},
		};
		const secondProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				secondCalls += 1;
				return { provider: providerId, fetchedAt: 2, limits: [] };
			},
		};
		const firstScope = {
			resolve: () => firstProvider,
			cacheKeyVersion: () => "same-source:1",
			providerIds: () => [],
		};
		const secondScope = {
			resolve: () => secondProvider,
			cacheKeyVersion: () => "same-source:1",
			providerIds: () => [],
		};

		try {
			await storage.reload();
			const unregisterFirst = storage.registerSessionUsageProviders("first-session", firstScope);
			const unregisterSecond = storage.registerSessionUsageProviders("second-session", secondScope);
			try {
				const firstPromise = storage.fetchUsageReports({ usageScopeId: "first-session" });
				await firstStarted;
				const secondPromise = storage.fetchUsageReports({ usageScopeId: "second-session" });
				releaseFirst();
				const [first, second] = await Promise.all([firstPromise, secondPromise]);
				expect(first?.[0]?.fetchedAt).toBe(1);
				expect(second?.[0]?.fetchedAt).toBe(1);
				expect(firstCalls).toBe(1);
				expect(secondCalls).toBe(0);
			} finally {
				unregisterSecond();
				unregisterFirst();
			}
		} finally {
			storage.close();
		}
	});

	it("ingests headers through a globally registered extension provider", async () => {
		const providerId = "header-global-usage";
		const report: UsageReport = { provider: providerId, fetchedAt: Date.now(), limits: [] };
		let storeIngests = 0;
		let fetchCalls = 0;
		const store: AuthCredentialStore = {
			...createStore([
				{
					id: 1,
					credential: {
						type: "oauth",
						access: "access-token",
						refresh: "refresh-token",
						expires: Date.now() + 3_600_000,
					},
					provider: providerId,
					disabledCause: null,
				},
			]),
			ingestUsageReport() {
				storeIngests += 1;
				return true;
			},
		};
		const registry = new UsageProviderRegistry();
		registry.register(
			{
				id: providerId,
				parseRateLimitHeaders: () => report,
				fetchUsage: async () => {
					fetchCalls += 1;
					return report;
				},
			},
			"extension:header-global",
		);
		const storage = new AuthStorage(store, { usageProviderRegistry: registry });

		try {
			await storage.reload();
			expect(storage.ingestUsageHeaders(providerId, {})).toBe(true);
			expect(storeIngests).toBe(0);
			expect((await storage.fetchUsageReports())?.[0]?.provider).toBe(providerId);
			expect(fetchCalls).toBe(0);
		} finally {
			storage.close();
		}
	});

	it("keeps extension header reports in the durable cache and records fetched history", async () => {
		const providerId = "header-scoped-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const report: UsageReport = {
			provider: providerId,
			fetchedAt: Date.now(),
			limits: [
				{
					id: `${providerId}:window`,
					label: "Scoped",
					scope: { provider: providerId, windowId: "window" },
					window: { id: "window", label: "Window" },
					amount: { usedFraction: 0.25, unit: "percent" },
					status: "ok",
				},
			],
		};
		let storeIngests = 0;
		let recordedEntries = 0;
		let fetchCalls = 0;
		const store: AuthCredentialStore = {
			...createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]),
			ingestUsageReport() {
				storeIngests += 1;
				return true;
			},
			recordUsageSnapshots(entries) {
				recordedEntries += entries.length;
			},
		};
		const storage = new AuthStorage(store);
		const provider: UsageProvider = {
			id: providerId,
			parseRateLimitHeaders: () => report,
			fetchUsage: async () => {
				fetchCalls += 1;
				return report;
			},
		};
		const unregister = storage.registerSessionUsageProviders("header-session", {
			resolve: () => provider,
			cacheKeyVersion: () => "header-source:1",
			providerIds: () => [],
		});

		try {
			await storage.reload();
			expect(
				storage.ingestUsageHeaders(
					providerId,
					{},
					{
						sessionId: "header-session",
						usageScopeId: "header-session",
					},
				),
			).toBe(true);
			expect(storeIngests).toBe(0);
			expect((await storage.fetchUsageReports({ usageScopeId: "header-session" }))?.[0]?.limits[0]?.label).toBe(
				"Scoped",
			);
			expect(fetchCalls).toBe(0);
			expect(recordedEntries).toBe(0);

			await storage.invalidateUsageCache(providerId);
			expect((await storage.fetchUsageReports({ usageScopeId: "header-session" }))?.[0]?.limits[0]?.label).toBe(
				"Scoped",
			);
			expect(fetchCalls).toBe(1);
			expect(recordedEntries).toBe(1);
		} finally {
			unregister();
			storage.close();
		}
	});

	it("reuses durable cache entries across process restart until the report TTL expires", async () => {
		const providerId = "reloaded-extension-usage";
		const store = createStore([
			{
				id: 1,
				provider: providerId,
				credential: { type: "api_key", key: "runtime-key" },
				disabledCause: null,
			},
		]);
		let firstCalls = 0;
		let secondCalls = 0;
		const firstProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				firstCalls += 1;
				return { provider: providerId, fetchedAt: 1, limits: [] };
			},
		};
		const secondProvider: UsageProvider = {
			id: providerId,
			fetchUsage: async () => {
				secondCalls += 1;
				return { provider: providerId, fetchedAt: 2, limits: [] };
			},
		};
		const firstStorage = new AuthStorage(store);
		await firstStorage.reload();
		firstStorage.syncExtensionUsageProviders(
			["extension:reload"],
			[{ provider: firstProvider, sourceId: "extension:reload" }],
		);
		expect((await firstStorage.fetchUsageReports())?.[0]?.fetchedAt).toBe(1);
		firstStorage.close();

		const secondStorage = new AuthStorage(store);
		try {
			await secondStorage.reload();
			secondStorage.syncExtensionUsageProviders(
				["extension:reload"],
				[{ provider: secondProvider, sourceId: "extension:reload" }],
			);
			expect((await secondStorage.fetchUsageReports())?.[0]?.fetchedAt).toBe(1);
			expect(firstCalls).toBe(1);
			expect(secondCalls).toBe(0);
		} finally {
			secondStorage.close();
		}
	});

	it("contains throwing extension supports callbacks", async () => {
		const providerId = "throwing-supports-usage";
		const storage = new AuthStorage(
			createStore([
				{
					id: 1,
					provider: providerId,
					credential: { type: "api_key", key: "runtime-key" },
					disabledCause: null,
				},
			]),
		);
		let fetchCalls = 0;
		const provider: UsageProvider = {
			id: providerId,
			supports() {
				throw new Error("supports failed");
			},
			async fetchUsage() {
				fetchCalls += 1;
				return { provider: providerId, fetchedAt: 1, limits: [] };
			},
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:throwing-supports"],
				[{ provider, sourceId: "extension:throwing-supports" }],
			);
			expect(await storage.fetchUsageReports()).toEqual([]);
			expect(fetchCalls).toBe(0);
		} finally {
			storage.close();
		}
	});

	it("contains throwing extension header parsers", async () => {
		const providerId = "throwing-header-parser";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		const provider: UsageProvider = {
			id: providerId,
			async fetchUsage() {
				return null;
			},
			parseRateLimitHeaders() {
				throw new Error("parser failed");
			},
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:throwing-parser"],
				[{ provider, sourceId: "extension:throwing-parser" }],
			);
			await storage.getApiKey(providerId, "header-session");
			expect(storage.ingestUsageHeaders(providerId, {}, { sessionId: "header-session" })).toBe(false);
		} finally {
			storage.close();
		}
	});

	it("rejects malformed and cross-provider usage reports", async () => {
		const providerId = "malformed-usage";
		const credential = {
			type: "oauth" as const,
			access: "access-token",
			refresh: "refresh-token",
			expires: Date.now() + 3_600_000,
			accountId: "account-1",
		};
		const storage = new AuthStorage(createStore([{ id: 1, provider: providerId, credential, disabledCause: null }]));
		const provider: UsageProvider = {
			id: providerId,
			async fetchUsage() {
				return { provider: providerId, fetchedAt: 1 } as unknown as UsageReport;
			},
			parseRateLimitHeaders() {
				return { provider: "wrong-provider", fetchedAt: 1, limits: [] };
			},
		};

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(["extension:malformed"], [{ provider, sourceId: "extension:malformed" }]);
			expect(await storage.fetchUsageReports()).toEqual([]);
			await storage.getApiKey(providerId, "header-session");
			expect(storage.ingestUsageHeaders(providerId, {}, { sessionId: "header-session" })).toBe(false);
		} finally {
			storage.close();
		}
	});

	it("keeps an observed usage-limit retry block credential-wide with extension providers", async () => {
		const providerId = "global-extension-block";
		const storage = new AuthStorage(
			createStore([
				{
					id: 1,
					provider: providerId,
					credential: { type: "api_key", key: "first-key" },
					disabledCause: null,
				},
				{
					id: 2,
					provider: providerId,
					credential: { type: "api_key", key: "second-key" },
					disabledCause: null,
				},
			]),
		);
		const provider = createUsageProvider(providerId);

		try {
			await storage.reload();
			storage.syncExtensionUsageProviders(
				["extension:global-block"],
				[{ provider, sourceId: "extension:global-block" }],
			);
			await storage.markUsageLimitReached(providerId, "first-session", {
				credentialId: 1,
				retryAfterMs: 60_000,
			});
			expect(await storage.getApiKey(providerId, "sibling-session")).toBe("second-key");
		} finally {
			storage.close();
		}
	});
});
