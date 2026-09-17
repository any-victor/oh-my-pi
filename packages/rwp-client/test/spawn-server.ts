import * as path from "node:path";
import type { ReadableStreamDefaultReader } from "node:stream/web";

const READY_STDOUT_RE = /^rwp-server listening on (http:\/\/\S+)$/m;
const READY_STDERR_RE = /\baddress=(127\.0\.0\.1:\d+)\b/;
const READY_TIMEOUT_MS = 60_000;
const SHUTDOWN_TIMEOUT_MS = 5_000;

export interface SpawnedRwpServer {
	baseUrl: string;
	token?: string;
	close(): Promise<void>;
}

function matchServerUrl(stdoutText: string, stderrText: string): string | undefined {
	const stdoutMatch = stdoutText.match(READY_STDOUT_RE);
	if (stdoutMatch?.[1]) {
		return stdoutMatch[1];
	}
	const stderrMatch = stderrText.match(READY_STDERR_RE);
	if (stderrMatch?.[1]) {
		return `http://${stderrMatch[1]}`;
	}
	return undefined;
}

async function waitForServerUrl(
	stdout: ReadableStream<Uint8Array>,
	stderr: ReadableStream<Uint8Array>,
): Promise<string> {
	const stdoutReader = stdout.getReader() as ReadableStreamDefaultReader<Uint8Array>;
	const stderrReader = stderr.getReader() as ReadableStreamDefaultReader<Uint8Array>;
	const stdoutDecoder = new TextDecoder();
	const stderrDecoder = new TextDecoder();
	let stdoutText = "";
	let stderrText = "";
	let settled = false;
	let closedReaders = 0;
	const { promise, resolve, reject } = Promise.withResolvers<string>();

	const timeoutId = setTimeout(() => {
		if (settled) {
			return;
		}
		settled = true;
		reject(
			new Error(
				`rwp-server did not announce its bind address within ${READY_TIMEOUT_MS}ms. stdout=${JSON.stringify(stdoutText)} stderr=${JSON.stringify(stderrText)}`,
			),
		);
	}, READY_TIMEOUT_MS);

	const maybeResolve = (): void => {
		if (settled) {
			return;
		}
		const baseUrl = matchServerUrl(stdoutText, stderrText);
		if (baseUrl !== undefined) {
			settled = true;
			resolve(baseUrl);
			return;
		}
		if (closedReaders === 2) {
			settled = true;
			reject(
				new Error(
					`rwp-server exited before announcing its bind address. stdout=${JSON.stringify(stdoutText)} stderr=${JSON.stringify(stderrText)}`,
				),
			);
		}
	};

	const pump = async (
		reader: ReadableStreamDefaultReader<Uint8Array>,
		decoder: TextDecoder,
		onChunk: (text: string) => void,
	): Promise<void> => {
		try {
			while (!settled) {
				const chunk = await reader.read();
				if (chunk.done) {
					closedReaders += 1;
					maybeResolve();
					return;
				}
				onChunk(decoder.decode(chunk.value, { stream: true }));
				maybeResolve();
			}
		} catch (error) {
			if (!settled) {
				settled = true;
				reject(error);
			}
		} finally {
			reader.releaseLock();
		}
	};

	void pump(stdoutReader, stdoutDecoder, text => {
		stdoutText += text;
	});
	void pump(stderrReader, stderrDecoder, text => {
		stderrText += text;
	});

	try {
		return await promise;
	} finally {
		clearTimeout(timeoutId);
	}
}

async function waitForExit(exited: Promise<number>, timeoutMs: number): Promise<boolean> {
	const result = await Promise.race([exited.then(() => true), Bun.sleep(timeoutMs).then(() => false)]);
	return result;
}

export async function spawnRwpServer(): Promise<SpawnedRwpServer> {
	const repoRoot = path.resolve(import.meta.dir, "..", "..", "..");
	const process = Bun.spawn(["cargo", "run", "--quiet", "-p", "rwp-server", "--", "--bind", "127.0.0.1:0"], {
		cwd: repoRoot,
		stdin: "ignore",
		stdout: "pipe",
		stderr: "pipe",
	});

	if (process.stdout === null || process.stderr === null) {
		process.kill("SIGKILL");
		await process.exited;
		throw new Error("rwp-server pipes are unavailable");
	}

	try {
		const baseUrl = await waitForServerUrl(process.stdout, process.stderr);
		return {
			baseUrl,
			async close(): Promise<void> {
				process.kill("SIGTERM");
				const exited = await waitForExit(process.exited, SHUTDOWN_TIMEOUT_MS);
				if (!exited) {
					process.kill("SIGKILL");
					await process.exited;
				}
			},
		};
	} catch (error) {
		process.kill("SIGKILL");
		await process.exited;
		throw error;
	}
}
