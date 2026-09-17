import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type { Server } from "bun";
import {
	BadRequestError,
	type BashEvent,
	computeLineHash,
	EtagMismatchError,
	type EvalEvent,
	type GrepRecord,
	type LspGetResponse,
	NotFoundError,
	RwpClient,
	type SessionEvent,
} from "../src";

interface SessionState {
	cwd: string;
	env: Record<string, string>;
	files: Map<string, { text: string; etag: string }>;
}

const sessionId = "session-1";
const evalName = "py-main";
const lspName = "main";
const initialFileText = "alpha\nbeta\n";
const initialFileEtag = "etag-1";
const updatedFileText = "alpha\ngamma\n";
const updatedFileEtag = "etag-2";

let server: Server<unknown>;
let client: RwpClient;
let evalX = 0;
let session: SessionState;

function ndjson(items: unknown[]): Response {
	return new Response(`${items.map(item => JSON.stringify(item)).join("\n")}\n`, {
		headers: { "content-type": "application/x-ndjson" },
	});
}

function json(body: unknown, init?: ResponseInit): Response {
	return Response.json(body, init);
}

function resetState(): void {
	evalX = 0;
	session = {
		cwd: "/repo",
		env: {},
		files: new Map([["src/foo.ts", { text: initialFileText, etag: initialFileEtag }]]),
	};
}

async function bodyJson(request: Request): Promise<unknown> {
	return await request.json();
}

beforeAll(() => {
	resetState();
	server = Bun.serve({
		port: 0,
		fetch: async request => {
			const url = new URL(request.url);
			const pathname = url.pathname;

			if (pathname === "/sessions" && request.method === "POST") {
				const body = (await bodyJson(request)) as { cwd?: string; env?: Record<string, string> };
				session.cwd = body.cwd ?? session.cwd;
				session.env = body.env ?? {};
				return json({ id: sessionId }, { status: 201 });
			}

			if (pathname === `/sessions/${sessionId}` && request.method === "DELETE") {
				return new Response(null, { status: 204 });
			}

			if (pathname === `/sessions/${sessionId}/cwd` && request.method === "PUT") {
				const body = (await bodyJson(request)) as { cwd: string };
				session.cwd = body.cwd;
				return new Response(null, { status: 204 });
			}

			if (pathname === `/sessions/${sessionId}/env` && request.method === "PATCH") {
				const body = (await bodyJson(request)) as { env: Record<string, string | null> };
				for (const [key, value] of Object.entries(body.env)) {
					if (value === null) {
						delete session.env[key];
					} else {
						session.env[key] = value;
					}
				}
				return new Response(null, { status: 204 });
			}

			if (pathname === `/sessions/${sessionId}/events` && request.method === "GET") {
				const events: SessionEvent[] = [
					{ type: "file-changed", path: "src/foo.ts", etag: updatedFileEtag },
					{ type: "heartbeat" },
				];
				return ndjson(events);
			}

			if (pathname === `/sessions/${sessionId}/read.lines` && request.method === "GET") {
				const path = url.searchParams.get("path");
				if (path !== "src/foo.ts") {
					return json({ code: "not-found", message: "missing" }, { status: 404 });
				}
				const file = session.files.get(path);
				if (file === undefined) {
					return json({ code: "not-found", message: "missing" }, { status: 404 });
				}
				return new Response(file.text, {
					headers: {
						"content-type": "text/plain; charset=utf-8",
						etag: `"${file.etag}"`,
						"x-total-lines": "2",
					},
				});
			}

			if (pathname === `/sessions/${sessionId}/write.lines` && request.method === "PUT") {
				const path = url.searchParams.get("path");
				const ifMatch = request.headers.get("if-match");
				if (path !== "src/foo.ts") {
					return json({ code: "not-found", message: "missing" }, { status: 404 });
				}
				if (ifMatch !== initialFileEtag) {
					return json({ code: "etag-mismatch", message: "etag mismatch" }, { status: 412 });
				}
				const text = await request.text();
				session.files.set(path, { text, etag: updatedFileEtag });
				return new Response(null, { status: 204, headers: { etag: `"${updatedFileEtag}"` } });
			}

			if (pathname === `/sessions/${sessionId}/grep` && request.method === "GET") {
				const records: GrepRecord[] = [
					{ path: "src/foo.ts", line: 1, kind: "context", text: "alpha" },
					{ path: "src/foo.ts", line: 2, kind: "match", text: "gamma" },
				];
				return ndjson(records);
			}

			if (pathname === `/sessions/${sessionId}/bash.exec` && request.method === "POST") {
				const events: BashEvent[] = [
					{ type: "output", data: "hi\n" },
					{ type: "exit", code: 0, cancelled: false, timed_out: false },
				];
				return ndjson(events);
			}

			if (pathname === `/eval/${evalName}` && request.method === "PUT") {
				return new Response(null, { status: 201 });
			}

			if (pathname === `/eval/${evalName}` && request.method === "GET") {
				return json({ name: evalName, lang: "python", status: "idle", ref_count: 0 });
			}

			if (pathname === `/eval/${evalName}` && request.method === "DELETE") {
				return new Response(null, { status: 204 });
			}

			if (pathname === `/eval/${evalName}` && request.method === "POST") {
				const body = (await bodyJson(request)) as { code: string };
				const code = body.code.trim();
				if (code === "x = 1") {
					evalX = 1;
					return ndjson([{ type: "status", state: "idle" }]);
				}
				if (code === "print(x)") {
					return ndjson([
						{ type: "stdout", data: `${evalX}\n` },
						{ type: "status", state: "idle" },
					]);
				}
				return ndjson([{ type: "status", state: "idle" }]);
			}

			if (pathname === `/lsp/${lspName}` && request.method === "PUT") {
				const body = (await bodyJson(request)) as { root_uri?: string };
				const response: LspGetResponse = {
					name: lspName,
					initialized: true,
					capabilities: { rootUri: body.root_uri ?? null, serverInfo: { name: "stub-lsp" } },
					ref_count: 0,
					last_active_ms: 5,
				};
				return json(response, { status: 201 });
			}

			if (pathname === `/lsp/${lspName}` && request.method === "GET") {
				return json({
					name: lspName,
					initialized: true,
					capabilities: { serverInfo: { name: "stub-lsp" } },
					ref_count: 0,
					last_active_ms: 10,
				});
			}

			if (pathname === `/lsp/${lspName}` && request.method === "DELETE") {
				return new Response(null, { status: 204 });
			}

			return json({ code: "not-found", message: `${pathname} not mocked` }, { status: 404 });
		},
		development: false,
	});
	client = new RwpClient({ baseUrl: `http://127.0.0.1:${server.port}` });
});

afterAll(() => {
	server.stop(true);
});

describe("RwpClient", () => {
	test("session lifecycle, read.lines, write.lines, events, grep, bash, eval, and lsp", async () => {
		resetState();
		const created = await client.createSession({ cwd: "/repo", env: { HELLO: "1" } });
		expect(created.id).toBe(sessionId);

		await created.setCwd({ cwd: "/repo/next" });
		await created.patchEnv({ env: { HELLO: null, WORLD: "2" } });
		expect(session.cwd).toBe("/repo/next");
		expect(session.env).toEqual({ WORLD: "2" });

		const lines = await created.readLines("src/foo.ts", { range: "1-2" });
		expect(lines.text).toBe(initialFileText);
		expect(lines.etag).toBe(initialFileEtag);
		expect(lines.totalLines).toBe(2);
		expect(lines.decorated()).toBe(
			`1${computeLineHash(1, "alpha")}|alpha\n2${computeLineHash(2, "beta")}|beta\n3${computeLineHash(3, "")}|`,
		);

		await expect(created.writeLines("src/foo.ts", updatedFileText, { ifMatch: "wrong" })).rejects.toBeInstanceOf(
			EtagMismatchError,
		);

		const nextEtag = await created.writeLines("src/foo.ts", updatedFileText, { ifMatch: initialFileEtag });
		expect(nextEtag).toBe(updatedFileEtag);
		expect(session.files.get("src/foo.ts")?.text).toBe(updatedFileText);

		const events = [] as SessionEvent[];
		for await (const event of await created.events()) {
			events.push(event);
		}
		expect(events).toEqual([
			{ type: "file-changed", path: "src/foo.ts", etag: updatedFileEtag },
			{ type: "heartbeat" },
		]);

		const grep = [] as GrepRecord[];
		for await (const record of await created.grep("gamma", { paths: ["src/foo.ts"], context: 1 })) {
			grep.push(record);
		}
		expect(grep.map(record => record.kind)).toEqual(["context", "match"]);
		expect(grep[1]?.text).toBe("gamma");

		const bash = [] as BashEvent[];
		for await (const event of await created.bashExec({ command: "echo hi" })) {
			bash.push(event);
		}
		expect(bash).toEqual([
			{ type: "output", data: "hi\n" },
			{ type: "exit", code: 0, cancelled: false, timed_out: false },
		]);

		await client.putEval(evalName, { kind: "eval", lang: "python" });
		expect(await client.getEval(evalName)).toEqual({ name: evalName, lang: "python", status: "idle", ref_count: 0 });
		const firstEval = [] as EvalEvent[];
		for await (const event of await client.execEval(evalName, { code: "x = 1" })) {
			firstEval.push(event);
		}
		expect(firstEval).toEqual([{ type: "status", state: "idle" }]);
		const secondEval = [] as EvalEvent[];
		for await (const event of await client.execEval(evalName, { code: "print(x)" })) {
			secondEval.push(event);
		}
		expect(secondEval).toEqual([
			{ type: "stdout", data: "1\n" },
			{ type: "status", state: "idle" },
		]);
		await client.deleteEval(evalName);

		const lspCreated = await client.putLsp(lspName, {
			kind: "lsp",
			command: "python3",
			args: ["stub.py"],
			root_uri: "file:///repo",
		});
		expect(lspCreated.initialized).toBe(true);
		expect(lspCreated.capabilities).toEqual({ rootUri: "file:///repo", serverInfo: { name: "stub-lsp" } });
		const lspStatus = await client.getLsp(lspName);
		expect(lspStatus.capabilities).toEqual({ serverInfo: { name: "stub-lsp" } });
		await client.deleteLsp(lspName);

		await created.delete();
	});

	test("maps not-found into typed errors", async () => {
		await expect(client.readLines(sessionId, "missing.ts")).rejects.toBeInstanceOf(NotFoundError);
	});

	test("exports bad-request subclass", () => {
		const error = new BadRequestError(400, { code: "bad-request", message: "bad" });
		expect(error.status).toBe(400);
	});
});
