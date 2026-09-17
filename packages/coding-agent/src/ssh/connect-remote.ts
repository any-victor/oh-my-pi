import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import * as path from "node:path";
import { logger } from "@oh-my-pi/pi-utils";
import { resolveRwpServerBinaryFor, type SupportedArch, type SupportedPlatform } from "../backend/rwp-server-path";
import type { BackendSelectOptions } from "../backend/select";
import type { SSHConnectionTarget } from "./connection-manager";
import { copyFileSCP, executeSSH, type SSHSpawnedProcess, spawnWithPortForward } from "./ssh-executor";

const REMOTE_BIN_DIR = "~/.omp/bin";
const POSIX_REMOTE_BINARY = `${REMOTE_BIN_DIR}/rwp-server`;
const WINDOWS_REMOTE_BINARY = `${REMOTE_BIN_DIR}/rwp-server.exe`;
const STARTUP_TIMEOUT_MS = 10_000;
const SHUTDOWN_TIMEOUT_MS = 5_000;
const MAX_START_ATTEMPTS = 3;
const LISTEN_LINE_RE = /rwp-server listening on https?:\/\/127\.0\.0\.1:(\d+)/i;
const VERSION_RE = /(?:^|\s)rwp-server\s+([^\s]+)/i;
const MAX_OUTPUT_SUMMARY_CHARS = 400;

export interface RemotePlatformTarget {
	platform: SupportedPlatform;
	arch: SupportedArch;
	id: "darwin-arm64" | "darwin-x64" | "linux-x64" | "linux-arm64" | "win32-x64";
	binaryPath: string;
}

export interface RemoteConnectionIdentity {
	transport: "ssh";
	sessionIdentity: string;
	displayTarget: string;
	displayCwd: string;
	remoteExecutionCwd?: string;
	target: {
		kind: "ssh";
		host: string;
		username?: string;
		port?: number;
		label: string;
	};
}

export interface RemoteConnectionResult {
	remote: NonNullable<BackendSelectOptions["remote"]>;
	host: SSHConnectionTarget;
	identity: RemoteConnectionIdentity;
	localPort: number;
	remotePort: number;
	platformTarget: RemotePlatformTarget;
	disposeRemoteConnection(): Promise<void>;
}

export interface ConnectRemoteDeps {
	executeSSH?: typeof executeSSH;
	copyFileSCP?: typeof copyFileSCP;
	spawnWithPortForward?: typeof spawnWithPortForward;
	allocatePort?: () => Promise<number>;
	readWorkspaceVersion?: () => Promise<string>;
	resolveBundledBinary?: (platform: SupportedPlatform, arch: SupportedArch) => string | null;
	randomToken?: () => string;
}

export function parseConnectTarget(input: string): { host: SSHConnectionTarget; path?: string } {
	const raw = input.trim();
	if (!raw) {
		throw new Error("--connect requires a non-empty SSH target");
	}

	if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) {
		const parsed = new URL(raw);
		if (parsed.protocol !== "ssh:") {
			throw new Error(`Unsupported --connect scheme: ${parsed.protocol}`);
		}
		if (!parsed.hostname) {
			throw new Error(`Invalid SSH target: ${input}`);
		}
		const port = parsed.port ? Number.parseInt(parsed.port, 10) : undefined;
		if (parsed.port && (!port || Number.isNaN(port) || port <= 0)) {
			throw new Error(`Invalid SSH port in target: ${input}`);
		}
		return {
			host: buildTarget({
				host: parsed.hostname,
				username: parsed.username || undefined,
				port,
			}),
			path: normalizeConnectPath(parsed.pathname),
		};
	}

	const colonIndex = raw.indexOf(":");
	const hostInput = colonIndex >= 0 ? raw.slice(0, colonIndex) : raw;
	const pathInput = colonIndex >= 0 ? raw.slice(colonIndex + 1) : undefined;
	const atIndex = hostInput.lastIndexOf("@");
	if (atIndex >= 0) {
		const username = hostInput.slice(0, atIndex).trim();
		const host = hostInput.slice(atIndex + 1).trim();
		if (!username || !host) {
			throw new Error(`Invalid SSH target: ${input}`);
		}
		return {
			host: buildTarget({ host, username }),
			path: normalizeConnectPath(pathInput),
		};
	}

	return {
		host: buildTarget({ host: hostInput.trim() }),
		path: normalizeConnectPath(pathInput),
	};
}

function buildTarget(parts: { host: string; username?: string; port?: number }): SSHConnectionTarget {
	if (!parts.host) {
		throw new Error("Invalid SSH target: missing host");
	}
	const name = `${parts.username ? `${parts.username}@` : ""}${parts.host}${parts.port ? `:${parts.port}` : ""}`;
	return {
		name,
		host: parts.host,
		username: parts.username,
		port: parts.port,
	};
}

export function formatSshSessionIdentity(host: SSHConnectionTarget, remotePath: string | undefined): string {
	const url = new URL("ssh://placeholder");
	url.hostname = host.host;
	if (host.username) url.username = host.username;
	if (host.port !== undefined) url.port = String(host.port);
	url.pathname = remotePath ?? "";
	return remotePath ? url.toString() : url.toString().replace(/\/$/, "");
}

export function formatSshDisplayCwd(host: SSHConnectionTarget, remotePath: string | undefined): string {
	return remotePath ? `${host.name}:${remotePath}` : `${host.name}:<remote default cwd>`;
}

function buildRemoteConnectionIdentity(target: { host: SSHConnectionTarget; path?: string }): RemoteConnectionIdentity {
	return {
		transport: "ssh",
		sessionIdentity: formatSshSessionIdentity(target.host, target.path),
		displayTarget: target.host.name,
		displayCwd: formatSshDisplayCwd(target.host, target.path),
		remoteExecutionCwd: target.path,
		target: {
			kind: "ssh",
			host: target.host.host,
			username: target.host.username,
			port: target.host.port,
			label: target.host.name,
		},
	};
}

function normalizeConnectPath(pathname: string | undefined): string | undefined {
	if (pathname === undefined) {
		return undefined;
	}
	const decodedPath = decodeURIComponent(pathname);
	if (!decodedPath || decodedPath === "/") {
		return undefined;
	}
	if (decodedPath.startsWith("~")) {
		throw new Error(`Tilde-prefixed remote paths are not yet supported: ${decodedPath}`);
	}
	if (!decodedPath.startsWith("/")) {
		throw new Error(`Path suffix must start with '/' or '~': ${decodedPath}`);
	}
	return decodedPath;
}

export function mapUnameToRemotePlatform(unameOutput: string): RemotePlatformTarget {
	const [os, arch] = unameOutput.trim().split(/\s+/, 2);
	if (os === "Darwin" && arch === "arm64") {
		return { platform: "darwin", arch: "arm64", id: "darwin-arm64", binaryPath: POSIX_REMOTE_BINARY };
	}
	if (os === "Darwin" && arch === "x86_64") {
		return { platform: "darwin", arch: "x64", id: "darwin-x64", binaryPath: POSIX_REMOTE_BINARY };
	}
	if (os === "Linux" && arch === "x86_64") {
		return { platform: "linux", arch: "x64", id: "linux-x64", binaryPath: POSIX_REMOTE_BINARY };
	}
	if (os === "Linux" && arch === "aarch64") {
		return { platform: "linux", arch: "arm64", id: "linux-arm64", binaryPath: POSIX_REMOTE_BINARY };
	}
	throw new Error(`Unsupported remote platform: ${unameOutput.trim() || "<empty>"}`);
}

export async function detectRemotePlatform(
	host: SSHConnectionTarget,
	deps: Pick<ConnectRemoteDeps, "executeSSH"> = {},
): Promise<RemotePlatformTarget> {
	const runSSH = deps.executeSSH ?? executeSSH;
	const unameResult = await runSSH(host, "uname -s -m", { timeout: STARTUP_TIMEOUT_MS });
	if (unameResult.exitCode === 0) {
		const line = firstNonEmptyLine(unameResult.output);
		if (line) {
			try {
				return mapUnameToRemotePlatform(line);
			} catch {
				// Fall through to Windows probe.
			}
		}
	}

	const windowsProbe = await runSSH(
		host,
		'powershell -NoProfile -Command "[System.Environment]::OSVersion.Platform"',
		{ timeout: STARTUP_TIMEOUT_MS },
	);
	if (windowsProbe.exitCode === 0 && firstNonEmptyLine(windowsProbe.output)) {
		return { platform: "win32", arch: "x64", id: "win32-x64", binaryPath: WINDOWS_REMOTE_BINARY };
	}

	const unameLine = firstNonEmptyLine(unameResult.output) ?? "<empty>";
	throw new Error(`Unsupported remote platform or architecture for ${host.name}: ${unameLine}`);
}

export async function connectRemote(sshUrl: string, deps: ConnectRemoteDeps = {}): Promise<RemoteConnectionResult> {
	const target = parseConnectTarget(sshUrl);
	const runSSH = deps.executeSSH ?? executeSSH;
	const scpFile = deps.copyFileSCP ?? copyFileSCP;
	const spawnForward = deps.spawnWithPortForward ?? spawnWithPortForward;
	const allocatePort = deps.allocatePort ?? allocateEphemeralPort;
	const readVersion = deps.readWorkspaceVersion ?? readWorkspaceVersion;
	const resolveBundledBinary = deps.resolveBundledBinary ?? resolveRwpServerBinaryFor;
	const token = deps.randomToken?.() ?? randomBytes(32).toString("hex");
	const identity = buildRemoteConnectionIdentity(target);
	const platformTarget = await detectRemotePlatform(target.host, { executeSSH: runSSH });
	const bundledVersion = await readVersion();
	const localBinary = resolveBundledBinary(platformTarget.platform, platformTarget.arch);
	if (!localBinary) {
		throw new Error(
			`No bundled rwp-server binary for ${platformTarget.id}; build via \`bun run scripts/ci-build-rwp-server.ts --only ${platformTarget.id}\` or install the package on a host that includes it.`,
		);
	}

	const installedVersion = await probeRemoteBinaryVersion(target.host, platformTarget, runSSH);
	if (installedVersion !== bundledVersion) {
		await installRemoteBinary(target.host, platformTarget, localBinary, bundledVersion, runSSH, scpFile);
	}

	for (let attempt = 1; attempt <= MAX_START_ATTEMPTS; attempt += 1) {
		const localPort = await allocatePort();
		const remotePort = localPort;
		const child = await spawnForward(target.host, buildRemoteLaunchCommand(platformTarget, token, remotePort), {
			localPort,
			remotePort,
		});
		try {
			const boundPort = await waitForStartup(child, remotePort);
			if (boundPort !== remotePort) {
				throw new Error(`Remote rwp-server bound ${boundPort}, expected ${remotePort}`);
			}
			logger.debug("Connected to remote rwp-server", {
				host: target.host.name,
				platform: platformTarget.id,
				port: localPort,
			});
			return {
				remote: {
					baseUrl: `http://127.0.0.1:${localPort}`,
					token,
					cwd: target.path,
				},
				host: target.host,
				identity,
				localPort,
				remotePort,
				platformTarget,
				disposeRemoteConnection: async () => {
					await shutdownForward(child, target.host.name, localPort);
				},
			};
		} catch (error) {
			await shutdownForward(child, target.host.name, localPort);
			if (attempt === MAX_START_ATTEMPTS) {
				throw error;
			}
		}
	}

	throw new Error(`Failed to start remote rwp-server for ${target.host.name}`);
}

async function installRemoteBinary(
	host: SSHConnectionTarget,
	platformTarget: RemotePlatformTarget,
	localBinary: string,
	bundledVersion: string,
	runSSH: typeof executeSSH,
	scpFile: typeof copyFileSCP,
): Promise<void> {
	await ensureRemoteBinDir(host, platformTarget, runSSH);
	await scpFile(host, localBinary, platformTarget.binaryPath);
	if (platformTarget.platform !== "win32") {
		const chmodResult = await runSSH(host, `chmod +x ${platformTarget.binaryPath}`, { timeout: STARTUP_TIMEOUT_MS });
		if (chmodResult.exitCode !== 0) {
			throw new Error(
				`Failed to mark remote rwp-server executable on ${host.name}: ${summarizeOutput(chmodResult.output)}`,
			);
		}
	}
	const installedVersion = await probeRemoteBinaryVersion(host, platformTarget, runSSH);
	if (installedVersion !== bundledVersion) {
		throw new Error(
			`Uploaded rwp-server to ${host.name}, but remote version is ${installedVersion ?? "unknown"} instead of ${bundledVersion}`,
		);
	}
}

async function ensureRemoteBinDir(
	host: SSHConnectionTarget,
	platformTarget: RemotePlatformTarget,
	runSSH: typeof executeSSH,
): Promise<void> {
	const command =
		platformTarget.platform === "win32"
			? "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force -Path (Join-Path $HOME '.omp/bin') | Out-Null\""
			: `mkdir -p ${REMOTE_BIN_DIR}`;
	const result = await runSSH(host, command, { timeout: STARTUP_TIMEOUT_MS });
	if (result.exitCode !== 0) {
		throw new Error(`Failed to create remote install directory on ${host.name}: ${summarizeOutput(result.output)}`);
	}
}

async function probeRemoteBinaryVersion(
	host: SSHConnectionTarget,
	platformTarget: RemotePlatformTarget,
	runSSH: typeof executeSSH,
): Promise<string | null> {
	const command =
		platformTarget.platform === "win32"
			? "powershell -NoProfile -Command \"$p = Join-Path $HOME '.omp/bin/rwp-server.exe'; if (Test-Path $p) { & $p --version } else { exit 1 }\""
			: `test -x ${platformTarget.binaryPath} && ${platformTarget.binaryPath} --version`;
	const result = await runSSH(host, command, { timeout: STARTUP_TIMEOUT_MS });
	if (result.exitCode !== 0) {
		return null;
	}
	return parseRwpVersion(result.output);
}

function buildRemoteLaunchCommand(platformTarget: RemotePlatformTarget, token: string, remotePort: number): string {
	if (platformTarget.platform === "win32") {
		return `powershell -NoProfile -Command "$env:RWP_TOKEN='${token}'; & (Join-Path $HOME '.omp/bin/rwp-server.exe') --bind 127.0.0.1:${remotePort}"`;
	}
	return `env RWP_TOKEN=${token} ${platformTarget.binaryPath} --bind 127.0.0.1:${remotePort}`;
}

function parseRwpVersion(output: string): string | null {
	return output.match(VERSION_RE)?.[1] ?? null;
}

async function waitForStartup(child: SSHSpawnedProcess, expectedPort: number): Promise<number> {
	const { promise, resolve, reject } = Promise.withResolvers<number>();
	let done = false;
	let stdoutBuffer = "";
	let stderrBuffer = "";

	const finish = (cb: () => void) => {
		if (done) return;
		done = true;
		cb();
	};

	const maybeResolve = () => {
		const port = parseListeningPort(`${stdoutBuffer}\n${stderrBuffer}`);
		if (port !== null) {
			finish(() => resolve(port));
		}
	};

	const pump = async (stream: ReadableStream<Uint8Array> | null, assign: (value: string) => void) => {
		if (!stream) return;
		const reader = stream.getReader();
		const decoder = new TextDecoder();
		let buffer = "";
		try {
			while (true) {
				const { done: streamDone, value } = await reader.read();
				if (streamDone) break;
				buffer = appendTail(buffer, decoder.decode(value, { stream: true }));
				assign(buffer);
				maybeResolve();
			}
		} finally {
			reader.releaseLock();
		}
	};

	void pump(child.stdout, value => {
		stdoutBuffer = value;
	});
	void pump(child.stderr, value => {
		stderrBuffer = value;
	});
	void child.exited
		.then(code => {
			finish(() =>
				reject(
					new Error(
						`Remote rwp-server exited before startup (code ${code}): ${summarizeOutput(`${stdoutBuffer}\n${stderrBuffer}`)}`,
					),
				),
			);
		})
		.catch(error => {
			finish(() => reject(error instanceof Error ? error : new Error(String(error))));
		});
	const timeout = setTimeout(() => {
		finish(() =>
			reject(
				new Error(
					`Timed out waiting for remote rwp-server to listen on 127.0.0.1:${expectedPort}: ${summarizeOutput(`${stdoutBuffer}\n${stderrBuffer}`)}`,
				),
			),
		);
	}, STARTUP_TIMEOUT_MS);
	timeout.unref?.();

	try {
		return await promise;
	} finally {
		clearTimeout(timeout);
	}
}

function parseListeningPort(output: string): number | null {
	const match = output.match(LISTEN_LINE_RE);
	if (!match?.[1]) {
		return null;
	}
	const port = Number.parseInt(match[1], 10);
	return Number.isNaN(port) ? null : port;
}

async function shutdownForward(child: SSHSpawnedProcess, hostName: string, localPort: number): Promise<void> {
	try {
		child.kill("SIGTERM");
	} catch {
		// Process already exited.
	}
	const exited = await Promise.race([
		child.exited.then(() => true).catch(() => true),
		Bun.sleep(SHUTDOWN_TIMEOUT_MS).then(() => false),
	]);
	if (!exited) {
		try {
			child.kill("SIGKILL");
		} catch {
			// Process already exited.
		}
		await child.exited.catch(() => undefined);
	}
	logger.debug("Disconnected remote rwp-server", { host: hostName, port: localPort });
}

async function allocateEphemeralPort(): Promise<number> {
	const { promise, resolve, reject } = Promise.withResolvers<number>();
	const server = createServer();
	server.unref();
	server.on("error", reject);
	server.listen(0, "127.0.0.1", () => {
		const address = server.address();
		if (!address || typeof address !== "object") {
			server.close();
			reject(new Error("Failed to allocate local port"));
			return;
		}
		server.close(error => {
			if (error) {
				reject(error);
				return;
			}
			resolve(address.port);
		});
	});
	return await promise;
}

async function readWorkspaceVersion(): Promise<string> {
	const cargoTomlPath = path.resolve(import.meta.dir, "../../../../Cargo.toml");
	const content = await Bun.file(cargoTomlPath).text();
	const sectionMatch = content.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
	if (!sectionMatch?.[1]) {
		throw new Error(`Failed to read workspace.package.version from ${cargoTomlPath}`);
	}
	return sectionMatch[1];
}

function appendTail(current: string, chunk: string): string {
	const next = `${current}${chunk}`;
	return next.length <= MAX_OUTPUT_SUMMARY_CHARS * 2 ? next : next.slice(-MAX_OUTPUT_SUMMARY_CHARS * 2);
}

function summarizeOutput(output: string): string {
	const singleLine = output.replace(/\s+/g, " ").trim();
	if (!singleLine) {
		return "no output";
	}
	return singleLine.length <= MAX_OUTPUT_SUMMARY_CHARS
		? singleLine
		: `${singleLine.slice(0, MAX_OUTPUT_SUMMARY_CHARS)}…`;
}

function firstNonEmptyLine(output: string): string | null {
	for (const line of output.split(/\r?\n/)) {
		const trimmed = line.trim();
		if (trimmed) {
			return trimmed;
		}
	}
	return null;
}
