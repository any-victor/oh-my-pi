import { afterAll, afterEach, expect, it } from "bun:test";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { TempDir } from "@oh-my-pi/pi-utils";
import { AgentRegistry } from "../../registry/agent-registry";
import type { AgentSession } from "../../session/agent-session";
import { ArtifactManager } from "../../session/artifacts";
import { AgentProtocolHandler } from "../agent-protocol";
import { parseInternalUrl } from "../parse";
import { resetRegisteredArtifactDirsForTests } from "../registry-helpers";

const tempDir = TempDir.createSync("omp-nested-agent-url-");

afterEach(() => {
	AgentRegistry.resetGlobalForTests();
	resetRegisteredArtifactDirsForTests();
});
afterAll(() => {
	tempDir.removeSync();
});

// Regression for #4650: agent:// must resolve a deeply nested subagent's output.
// The write path (task/index.ts) writes each level's children one directory
// deeper via that level's own session file, while the read path historically
// searched only the flat, root-adopted ArtifactManager dir — so a deeply
// nested subagent (spawn depth >= 2) had output unreachable ("Not found / Available: none").
it("resolves a deeply nested subagent's output while its session is live and artifact-manager-adopted", async () => {
	const root = tempDir.path();
	const rootSessionFile = path.join(root, "session.jsonl");
	const rootArtifactsDir = rootSessionFile.slice(0, -6);
	await fs.mkdir(rootArtifactsDir, { recursive: true });
	// Every subagent adopts the root's ArtifactManager and reports its dir.
	const sharedArtifactManager = new ArtifactManager(rootArtifactsDir);

	// A mid-level subagent writes its own children one dir deeper (task/index.ts:sessionFile.slice(0,-6)).
	const midSessionFile = path.join(rootArtifactsDir, "CodexDeepDive.jsonl");
	const midOwnArtifactsDir = midSessionFile.slice(0, -6);
	await fs.mkdir(midOwnArtifactsDir, { recursive: true });

	const grandchildId = "CodexDeepDive.GraphStore";
	const grandchildSessionFile = path.join(midOwnArtifactsDir, `${grandchildId}.jsonl`);
	await fs.writeFile(path.join(midOwnArtifactsDir, `${grandchildId}.md`), "full report content");

	// Only getArtifactsDir() is exercised by the resolver; cast avoids constructing a full AgentSession.
	const fakeSession = {
		sessionManager: { getArtifactsDir: () => sharedArtifactManager.dir },
	} as unknown as AgentSession;

	const registry = AgentRegistry.global();
	registry.register({
		id: "Main",
		displayName: "main",
		kind: "main",
		session: fakeSession,
		sessionFile: rootSessionFile,
	});
	registry.register({
		id: "CodexDeepDive",
		displayName: "sub",
		kind: "sub",
		parentId: "Main",
		session: fakeSession,
		sessionFile: midSessionFile,
	});
	registry.register({
		id: grandchildId,
		displayName: "sub",
		kind: "sub",
		parentId: "CodexDeepDive",
		session: fakeSession,
		sessionFile: grandchildSessionFile,
	});

	const resource = await new AgentProtocolHandler().resolve(parseInternalUrl(`agent://${grandchildId}`));
	expect(resource.content).toBe("full report content");
});
