import { ptree } from "@oh-my-pi/pi-utils";
import {
	buildRemoteCommand,
	ensureConnection,
	ensureSshControlDir,
	getControlPathTemplate,
	type SSHConnectionTarget,
	supportsSshControlMaster,
} from "./connection-manager";
import { buildSshTarget } from "./utils";

export interface SSHExecutorOptions {
	timeout?: number;
	signal?: AbortSignal;
}

export interface SSHResult {
	output: string;
	exitCode: number | undefined;
	cancelled: boolean;
}

export interface SSHPortForwardOptions {
	localPort: number;
	remotePort: number;
	signal?: AbortSignal;
}

export interface SSHSpawnedProcess {
	stdout: ReadableStream<Uint8Array> | null;
	stderr: ReadableStream<Uint8Array> | null;
	exited: Promise<number>;
	kill(signal?: number | NodeJS.Signals): number | undefined;
	pid?: number;
}

function combineOutput(stdout: string, stderr: string): string {
	if (!stdout) return stderr;
	if (!stderr) return stdout;
	return `${stdout}${stdout.endsWith("\n") || stderr.startsWith("\n") ? "" : "\n"}${stderr}`;
}

async function collectStreamText(stream: ReadableStream<Uint8Array> | null | undefined): Promise<string> {
	if (!stream) return "";
	return await new Response(stream).text();
}

/**
 * Shared ControlMaster argv (no remote command). Passwords are never requested:
 * BatchMode refuses interactive auth.
 */
function buildControlMasterArgs(host: SSHConnectionTarget, options?: { includeStdinNull?: boolean }): string[] {
	const args = options?.includeStdinNull === false ? [] : ["-n"];
	if (supportsSshControlMaster()) {
		args.push("-o", "ControlMaster=auto", "-o", `ControlPath=${getControlPathTemplate()}`, "-o", "ControlPersist=3600");
	}
	args.push("-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new");
	if (host.port) {
		args.push("-p", String(host.port));
	}
	if (host.keyPath) {
		args.push("-i", host.keyPath);
	}
	return args;
}

function wrapSpawnedProcess(proc: ReturnType<typeof ptree.spawn>): SSHSpawnedProcess {
	return {
		stdout: proc.stdout as ReadableStream<Uint8Array> | null,
		stderr: proc.stderr as ReadableStream<Uint8Array> | null,
		exited: proc.exited,
		kill: (signal?: number | NodeJS.Signals) => {
			try {
				if (signal === "SIGKILL" || signal === 9) {
					proc.kill(undefined, -1);
				} else {
					proc.kill();
				}
			} catch {
				// Process already exited.
			}
			return proc.proc.pid;
		},
		pid: proc.proc.pid,
	};
}

export async function executeSSH(
	host: SSHConnectionTarget,
	command: string,
	options?: SSHExecutorOptions,
): Promise<SSHResult> {
	await ensureConnection(host);
	using child = ptree.spawn(["ssh", ...(await buildRemoteCommand(host, command))], {
		signal: options?.signal,
		timeout: options?.timeout,
		stdin: "pipe",
		stderr: "full",
	});
	child.nothrow();

	const stdoutPromise = collectStreamText(child.stdout);
	const stderrPromise = collectStreamText(child.stderr);

	try {
		const [stdout, stderr, exitCode] = await Promise.all([stdoutPromise, stderrPromise, child.exited]);
		return {
			output: combineOutput(stdout, stderr),
			exitCode,
			cancelled: false,
		};
	} catch (error) {
		const [stdout, stderr] = await Promise.all([stdoutPromise, stderrPromise]);
		if (error instanceof ptree.Exception) {
			return {
				output: combineOutput(stdout, stderr),
				exitCode: error.aborted ? undefined : error.exitCode,
				cancelled: error.aborted,
			};
		}
		throw error;
	}
}

export async function copyFileSCP(host: SSHConnectionTarget, localPath: string, remotePath: string): Promise<void> {
	await ensureConnection(host);
	ensureSshControlDir();
	const args = ["-O", ...buildControlMasterArgs({ ...host, port: undefined }, { includeStdinNull: false })];
	if (host.port) {
		args.push("-P", String(host.port));
	}
	args.push(localPath, `${buildSshTarget(host.username, host.host)}:${remotePath}`);
	const child = Bun.spawn(["scp", ...args], {
		stdout: "pipe",
		stderr: "pipe",
	});
	const [stdout, stderr, exitCode] = await Promise.all([
		collectStreamText(child.stdout),
		collectStreamText(child.stderr),
		child.exited,
	]);
	if (exitCode !== 0) {
		const detail = [stdout.trim(), stderr.trim()].filter(Boolean).join("\n");
		throw new Error(`scp upload failed${detail ? `: ${detail}` : ""}`);
	}
}

export async function spawnWithPortForward(
	host: SSHConnectionTarget,
	command: string,
	options: SSHPortForwardOptions,
): Promise<SSHSpawnedProcess> {
	await ensureConnection(host);
	ensureSshControlDir();
	const args = [
		...buildControlMasterArgs(host),
		"-o",
		"ExitOnForwardFailure=yes",
		"-o",
		"ServerAliveInterval=30",
		"-o",
		"ServerAliveCountMax=3",
		"-L",
		`${options.localPort}:127.0.0.1:${options.remotePort}`,
		buildSshTarget(host.username, host.host),
		command,
	];
	// Do not `using`-dispose: the forwarded rwp-server must outlive this call.
	const proc = ptree.spawn(["ssh", ...args], {
		signal: options.signal,
		stdin: "pipe",
		stderr: "full",
	});
	return wrapSpawnedProcess(proc);
}
