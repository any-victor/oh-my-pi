import { existsSync } from "node:fs";
import * as path from "node:path";
import { astToString, type OpenAPI3, default as openapiTS } from "openapi-typescript";

const SERVER_READY_RE = /^rwp-server listening on (http:\/\/\S+)$/m;
const READY_TIMEOUT_MS = 30_000;
const GENERATED_HEADER = ["/* eslint-disable */", "/* biome-ignore-all lint: generated file */", ""].join("\n");

const packageDir = path.resolve(import.meta.dir, "..");
const repoRoot = path.resolve(packageDir, "..", "..");
const outputPath = path.join(packageDir, "src", "generated.ts");

interface CargoMetadata {
	target_directory: string;
}

async function cargoMetadata(): Promise<CargoMetadata> {
	const proc = Bun.spawn(["cargo", "metadata", "--format-version", "1", "--no-deps"], {
		cwd: repoRoot,
		stdin: "ignore",
		stdout: "pipe",
		stderr: "pipe",
	});
	const [stdout, stderr, exitCode] = await Promise.all([
		new Response(proc.stdout).text(),
		new Response(proc.stderr).text(),
		proc.exited,
	]);
	if (exitCode !== 0) {
		throw new Error(`cargo metadata failed with exit code ${exitCode}: ${stderr}`);
	}
	return JSON.parse(stdout) as CargoMetadata;
}

async function ensureServerBinary(): Promise<void> {
	const metadata = await cargoMetadata();
	const serverBinary = path.join(
		metadata.target_directory,
		"debug",
		process.platform === "win32" ? "rwp-server.exe" : "rwp-server",
	);
	if (existsSync(serverBinary)) {
		return;
	}
	const build = Bun.spawn(["cargo", "build", "-p", "rwp-server"], {
		cwd: repoRoot,
		stdin: "ignore",
		stdout: "inherit",
		stderr: "inherit",
	});
	const exitCode = await build.exited;
	if (exitCode !== 0) {
		throw new Error(`cargo build failed with exit code ${exitCode}`);
	}
}

async function waitForServerUrl(
	stdout: ReadableStream<Uint8Array>,
	stderr: ReadableStream<Uint8Array>,
): Promise<string> {
	const stdoutReader = stdout.getReader();
	const stderrTextPromise = new Response(stderr).text();
	const decoder = new TextDecoder();
	let buffer = "";
	const deadline = Date.now() + READY_TIMEOUT_MS;

	try {
		while (Date.now() < deadline) {
			const remainingMs = deadline - Date.now();
			const chunk = await Promise.race([
				stdoutReader.read(),
				Bun.sleep(Math.max(1, remainingMs)).then(() => ({ done: true, value: undefined })),
			]);
			if (chunk.done) {
				break;
			}
			buffer += decoder.decode(chunk.value, { stream: true });
			const match = buffer.match(SERVER_READY_RE);
			if (match?.[1]) {
				return match[1];
			}
		}
	} finally {
		stdoutReader.releaseLock();
	}

	const stderrText = await stderrTextPromise;
	throw new Error(
		`rwp-server did not announce its bind address. stdout=${JSON.stringify(buffer)} stderr=${JSON.stringify(stderrText)}`,
	);
}

async function main(): Promise<void> {
	await ensureServerBinary();

	const server = Bun.spawn(["cargo", "run", "-p", "rwp-server", "--", "--bind", "127.0.0.1:0"], {
		cwd: repoRoot,
		stdin: "ignore",
		stdout: "pipe",
		stderr: "pipe",
	});

	try {
		if (server.stdout === null || server.stderr === null) {
			throw new Error("rwp-server pipes are unavailable");
		}
		const baseUrl = await waitForServerUrl(server.stdout, server.stderr);
		const response = await fetch(new URL("/openapi.json", baseUrl), {
			headers: { accept: "application/json" },
		});
		if (!response.ok) {
			throw new Error(`openapi.json request failed with ${response.status}`);
		}
		const schema = (await response.json()) as OpenAPI3;
		const ast = await openapiTS(schema);
		await Bun.write(outputPath, `${GENERATED_HEADER}${astToString(ast)}`);
	} finally {
		server.kill();
		await server.exited;
	}
}

await main();
