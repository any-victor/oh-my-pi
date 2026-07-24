import { describe, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { UsageProvider } from "@oh-my-pi/pi-ai";
import {
	collectExtensionUsageProviderRegistrations,
	ExtensionRuntime,
	loadExtensionFromFactory,
	loadExtensions,
} from "@oh-my-pi/pi-coding-agent/extensibility/extensions/loader";
import { EventBus } from "@oh-my-pi/pi-coding-agent/utils/event-bus";

describe("extension usage provider registration", () => {
	it("stores each factory's exact usage provider under its extension path", async () => {
		const firstProvider: UsageProvider = {
			id: "anthropic",
			fetchUsage: async () => null,
		};
		const secondProvider: UsageProvider = {
			id: "openai-codex",
			fetchUsage: async () => null,
		};
		const runtime = new ExtensionRuntime();
		const eventBus = new EventBus();
		const firstPath = "/extensions/first.ts";
		const secondPath = "/extensions/second.ts";

		const firstExtension = await loadExtensionFromFactory(
			pi => {
				pi.registerUsageProvider(firstProvider);
			},
			"/workspace",
			eventBus,
			runtime,
			firstPath,
		);
		const secondExtension = await loadExtensionFromFactory(
			pi => {
				pi.registerUsageProvider(secondProvider);
			},
			"/workspace",
			eventBus,
			runtime,
			secondPath,
		);

		const registrations = collectExtensionUsageProviderRegistrations([firstExtension, secondExtension]);
		expect(registrations.map(registration => registration.sourceId)).toEqual([firstPath, secondPath]);
		expect(registrations[0]?.provider).toBe(firstProvider);
		expect(registrations[1]?.provider).toBe(secondProvider);
	});

	it("discards usage providers when a factory fails", async () => {
		const retainedProvider: UsageProvider = {
			id: "anthropic",
			fetchUsage: async () => null,
		};
		const leakedProvider: UsageProvider = {
			id: "openai-codex",
			fetchUsage: async () => null,
		};
		const runtime = new ExtensionRuntime();
		const eventBus = new EventBus();
		const retainedExtension = await loadExtensionFromFactory(
			pi => {
				pi.registerUsageProvider(retainedProvider);
			},
			"/workspace",
			eventBus,
			runtime,
			"/extensions/retained.ts",
		);

		await expect(
			loadExtensionFromFactory(
				pi => {
					pi.registerUsageProvider(leakedProvider);
					throw new Error("factory failed");
				},
				"/workspace",
				eventBus,
				runtime,
				"/extensions/failed.ts",
			),
		).rejects.toThrow("factory failed");

		expect(collectExtensionUsageProviderRegistrations([retainedExtension])).toEqual([
			{ provider: retainedProvider, sourceId: "/extensions/retained.ts" },
		]);
	});

	it("continues loading after a failed path without retaining its usage provider", async () => {
		const tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "extension-usage-rollback-"));
		const failedPath = path.join(tempDir, "failed.ts");
		const succeedingPath = path.join(tempDir, "succeeding.ts");
		await Bun.write(
			failedPath,
			`export default function (pi) {
				pi.registerUsageProvider({ id: "failed-usage", fetchUsage: async () => null });
				throw new Error("failed after registration");
			}`,
		);
		await Bun.write(
			succeedingPath,
			`export default function (pi) {
				pi.registerUsageProvider({ id: "working-usage", fetchUsage: async () => null });
			}`,
		);
		try {
			const result = await loadExtensions([failedPath, succeedingPath], tempDir, new EventBus());
			expect(result.errors).toHaveLength(1);
			expect(result.extensions.map(extension => extension.path)).toEqual([succeedingPath]);
			const registrations = collectExtensionUsageProviderRegistrations(result.extensions);
			expect(registrations).toHaveLength(1);
			expect(registrations[0]).toMatchObject({
				sourceId: succeedingPath,
				provider: { id: "working-usage" },
			});
		} finally {
			await fs.rm(tempDir, { recursive: true, force: true });
		}
	});
});
