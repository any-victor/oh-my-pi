/**
 * Regression: advisor tool instances are built once and shared across every
 * roster member, then bound per invocation through an AsyncLocalStorage so the
 * concurrent advisors each observe their OWN provider UUID and local session
 * label. Without the async-local run, a delayed execution would read whichever
 * identity the last binder installed, leaking one advisor's provider session
 * into another's tool call.
 */

import { describe, expect, it } from "bun:test";
import { AsyncLocalStorage } from "node:async_hooks";
import { bindToolsToAsyncSessionIdentity, type ToolSessionIdentity } from "@oh-my-pi/pi-coding-agent";
import type { Tool } from "@oh-my-pi/pi-coding-agent/tools";

describe("bindToolsToAsyncSessionIdentity", () => {
	it("keeps each concurrent binding on its own provider and label identity", async () => {
		const identity = new AsyncLocalStorage<ToolSessionIdentity>();
		const scheduled: Array<PromiseWithResolvers<void>> = [];
		const observed: ToolSessionIdentity[] = [];

		const sharedTool = {
			name: "probe",
			async execute() {
				// Block until every peer has entered, THEN read identity after the
				// await. AsyncLocalStorage restores the caller's store across the
				// suspension; a mutable shared field would collapse to the last
				// binder's identity here.
				const gate = Promise.withResolvers<void>();
				scheduled.push(gate);
				await gate.promise;
				const entry = identity.getStore();
				if (entry) observed.push(entry);
				return { content: [] };
			},
		} as unknown as Tool;

		const first = bindToolsToAsyncSessionIdentity([sharedTool], identity, {
			providerSessionId: "provider-a",
			sessionLabel: "session-a-advisor",
		});
		const second = bindToolsToAsyncSessionIdentity([sharedTool], identity, {
			providerSessionId: "provider-b",
			sessionLabel: "session-b-advisor-reviewer",
		});

		// Binding produces a distinct per-invocation wrapper, not a mutation of the shared tool.
		expect(first[0]).not.toBe(second[0]);

		const firstDone = first[0]!.execute("call-1", {});
		const secondDone = second[0]!.execute("call-2", {});

		// Release in reverse entry order to prove the identity survives the await.
		while (scheduled.length < 2) await Promise.resolve();
		scheduled.pop()?.resolve();
		scheduled.pop()?.resolve();
		await Promise.all([firstDone, secondDone]);

		expect(observed).toHaveLength(2);
		const byProvider = new Map(observed.map(entry => [entry.providerSessionId, entry.sessionLabel]));
		expect(byProvider.get("provider-a")).toBe("session-a-advisor");
		expect(byProvider.get("provider-b")).toBe("session-b-advisor-reviewer");
	});

	it("preserves prototype metadata for provider-visible tool specs", () => {
		const identity = new AsyncLocalStorage<ToolSessionIdentity>();
		const tool = Object.assign(
			Object.create({
				get description(): string {
					return "probe tool";
				},
			}),
			{
				name: "probe",
				label: "Probe",
				parameters: { type: "object" },
				async execute() {
					return { content: [] };
				},
			},
		) as Tool;
		const [bound] = bindToolsToAsyncSessionIdentity([tool], identity, {
			providerSessionId: "provider-a",
			sessionLabel: "session-a-advisor",
		});
		// The agent loop spreads the bound tool then reads description separately.
		const spec = { ...bound!, description: bound!.description };
		expect(spec.name).toBe("probe");
		expect(spec.label).toBe("Probe");
		expect(spec.description).toBe("probe tool");
		expect(spec.parameters).toBe(tool.parameters);
		expect(spec.execute).not.toBe(tool.execute);
	});

	it("reads branded prototype getters from the original tool instance", () => {
		class BrandedTool {
			#description = "private probe tool";
			name = "private-probe";
			label = "Private Probe";
			parameters = { type: "object" };

			get description(): string {
				return this.#description;
			}

			async execute() {
				return { content: [] };
			}
		}

		const identity = new AsyncLocalStorage<ToolSessionIdentity>();
		const tool = new BrandedTool() as unknown as Tool;
		const [bound] = bindToolsToAsyncSessionIdentity([tool], identity, {
			providerSessionId: "provider-a",
			sessionLabel: "session-a-advisor",
		});
		const spec = { ...bound!, description: bound!.description };

		expect(spec.name).toBe("private-probe");
		expect(spec.label).toBe("Private Probe");
		expect(spec.description).toBe("private probe tool");
		expect(spec.parameters).toBe(tool.parameters);
	});
});
