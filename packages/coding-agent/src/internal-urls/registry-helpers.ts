/**
 * Shared helpers for internal-url protocol handlers that resolve IDs against
 * registered agent sessions.
 */
import { AgentRegistry } from "../registry/agent-registry";

const extraArtifactsDirs = new Set<string>();

export function registerArtifactsDir(dir: string): () => void {
	extraArtifactsDirs.add(dir);
	return () => {
		extraArtifactsDirs.delete(dir);
	};
}

export function resetRegisteredArtifactDirsForTests(): void {
	extraArtifactsDirs.clear();
}

/**
 * Snapshot of artifacts dirs for every registered session, deduped.
 *
 * Collects BOTH candidate dirs per ref: the live adopted
 * `sessionManager.getArtifactsDir()` (subagents adopt their parent's
 * `ArtifactManager` and report the parent's dir there) AND the ref's own
 * session-file dir (`.jsonl` suffix stripped). These agree for a root-spawned
 * subagent (dedup collapses them), but diverge for a deeply nested subagent
 * (spawn depth >= 2): `task/index.ts` writes each level's children one dir
 * deeper via the session file, while the adopted manager stays flat at the
 * root — so a deeply nested subagent's output lives only under the
 * session-file dir. Searching both lets `agent://` resolve output at any spawn
 * depth. Dedup keeps the common case to one entry.
 */
export function artifactsDirsFromRegistry(): string[] {
	const dirs: string[] = [];
	const addDir = (dir: string | null | undefined) => {
		if (!dir) return;
		if (!dirs.includes(dir)) dirs.push(dir);
	};
	for (const ref of AgentRegistry.global().list()) {
		addDir(ref.session?.sessionManager.getArtifactsDir());
		if (ref.sessionFile) addDir(ref.sessionFile.slice(0, -6));
	}
	for (const dir of extraArtifactsDirs) addDir(dir);
	return dirs;
}
