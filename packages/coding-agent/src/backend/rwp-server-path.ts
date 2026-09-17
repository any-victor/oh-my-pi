import * as fs from "node:fs";
import { createRequire } from "node:module";
import * as path from "node:path";

export type SupportedPlatform = "darwin" | "linux" | "win32";
export type SupportedArch = "x64" | "arm64";

const require = createRequire(import.meta.url);
const SUPPORTED_PLATFORMS = new Set<SupportedPlatform>(["darwin", "linux", "win32"]);
const SUPPORTED_ARCHES = new Set<SupportedArch>(["x64", "arm64"]);

function isSupportedPlatform(platform: string): platform is SupportedPlatform {
	return SUPPORTED_PLATFORMS.has(platform as SupportedPlatform);
}

function isSupportedArch(arch: string): arch is SupportedArch {
	return SUPPORTED_ARCHES.has(arch as SupportedArch);
}

function binaryFilename(platform: SupportedPlatform, arch: SupportedArch): string {
	return `rwp-server-${platform}-${arch}${platform === "win32" ? ".exe" : ""}`;
}

export function resolveLocalRwpServerBinary(): string | null {
	if (!isSupportedPlatform(process.platform) || !isSupportedArch(process.arch)) {
		return null;
	}

	return resolveRwpServerBinaryFor(process.platform, process.arch);
}

export function resolveRwpServerBinaryFor(platform: SupportedPlatform, arch: SupportedArch): string | null {
	const filename = binaryFilename(platform, arch);
	const bundled = path.resolve(import.meta.dir, "../../binaries", filename);
	if (fs.existsSync(bundled)) {
		return bundled;
	}

	try {
		return require.resolve(
			`@oh-my-pi/rwp-server-${platform}-${arch}/rwp-server${platform === "win32" ? ".exe" : ""}`,
		);
	} catch {
		return null;
	}
}

export function listBundledRwpServerBinaries(): Array<{
	platform: SupportedPlatform;
	arch: SupportedArch;
	path: string;
}> {
	const binariesDir = path.resolve(import.meta.dir, "../../binaries");
	let entries: string[];
	try {
		entries = fs.readdirSync(binariesDir).sort();
	} catch {
		return [];
	}

	const binaries: Array<{ platform: SupportedPlatform; arch: SupportedArch; path: string }> = [];
	for (const entry of entries) {
		const match = /^rwp-server-(darwin|linux|win32)-(x64|arm64)(\.exe)?$/.exec(entry);
		if (!match) {
			continue;
		}

		const [, platform, arch] = match;
		if (!isSupportedPlatform(platform) || !isSupportedArch(arch)) {
			continue;
		}

		binaries.push({
			platform,
			arch,
			path: path.join(binariesDir, entry),
		});
	}

	return binaries;
}
