// Gate: RWP_LIVE_TEST=1 bun --cwd packages/rwp-client run test
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import type { BashEvent, GrepRecord, ReadDbResponse, RwpSession, SqliteTablesResponse } from "../src";
import { EtagMismatchError, RwpClient } from "../src";
import type { SpawnedRwpServer } from "./spawn-server";
import { spawnRwpServer } from "./spawn-server";

type ReadDbResult = ReadDbResponse | SqliteTablesResponse;

function assertSession(value: RwpSession | undefined): RwpSession {
	if (value === undefined) {
		throw new Error("live test session is not initialized");
	}
	return value;
}

function assertTables(value: ReadDbResult): SqliteTablesResponse {
	if (!("tables" in value)) {
		throw new Error("expected sqlite tables response");
	}
	return value;
}

function assertRows(value: ReadDbResult): ReadDbResponse {
	if (!("rows" in value)) {
		throw new Error("expected sqlite rows response");
	}
	return value;
}

async function collect<T>(iterable: AsyncIterable<T>): Promise<T[]> {
	const values = [] as T[];
	for await (const value of iterable) {
		values.push(value);
	}
	return values;
}

function bashOutput(events: BashEvent[]): string {
	let output = "";
	for (const event of events) {
		if (event.type === "output") {
			output += event.data;
		}
	}
	return output;
}

async function requestLive(server: SpawnedRwpServer, pathname: string, init?: RequestInit): Promise<Response> {
	const headers = new Headers(init?.headers);
	if (server.token) {
		headers.set("authorization", `Bearer ${server.token}`);
	}
	return fetch(new URL(pathname, server.baseUrl), { ...init, headers });
}

describe.skipIf(process.env.RWP_LIVE_TEST !== "1")("RwpClient live integration", () => {
	let server: SpawnedRwpServer | undefined;
	let client: RwpClient | undefined;
	let session: RwpSession | undefined;
	let tempDir: string | undefined;

	beforeAll(async () => {
		server = await spawnRwpServer();
		client = new RwpClient({ baseUrl: server.baseUrl, token: server.token });
		tempDir = await mkdtemp(path.join(tmpdir(), "rwp-client-live-"));
		session = await client.createSession({ cwd: tempDir, env: {} });
	});

	afterAll(async () => {
		if (session !== undefined && server !== undefined) {
			await requestLive(server, `/sessions/${session.id}`, { method: "DELETE" });
		}
		if (server !== undefined) {
			await server.close();
			client = undefined;
		}
		if (tempDir !== undefined) {
			await rm(tempDir, { recursive: true, force: true });
		}
	});

	test("POST /sessions returns an id", () => {
		expect(assertSession(session).id.length).toBeGreaterThan(0);
	});

	test("PUT /sessions/{id}/cwd updates cwd", async () => {
		const liveSession = assertSession(session);
		const liveTempDir = tempDir;
		if (liveTempDir === undefined) {
			throw new Error("live temp dir is not initialized");
		}

		await liveSession.setCwd({ cwd: liveTempDir });
		const bash = await collect(await liveSession.bashExec({ command: "pwd", pty: false }));
		expect(bashOutput(bash)).toBe(`${liveTempDir}\n`);
		expect(bash.at(-1)?.type).toBe("exit");
	});

	test("PATCH /sessions/{id}/env sets a known variable", async () => {
		const liveSession = assertSession(session);

		await liveSession.patchEnv({ env: { RWP_LIVE_ENV: "set-from-live-test" } });
		const bash = await collect(await liveSession.bashExec({ command: 'printf "%s\\n" "$RWP_LIVE_ENV"', pty: false }));
		expect(bashOutput(bash)).toBe("set-from-live-test\n");
		expect(bash.at(-1)?.type).toBe("exit");
	});

	test("POST /sessions/{id}/read.lines reads a created file", async () => {
		const liveSession = assertSession(session);
		const liveTempDir = tempDir;
		if (liveTempDir === undefined) {
			throw new Error("live temp dir is not initialized");
		}

		await writeFile(path.join(liveTempDir, "read-lines.txt"), "alpha\nbeta\ngamma\n");
		const lines = await liveSession.readLines("read-lines.txt", { range: "1-2" });
		expect(lines.text).toBe("alpha\nbeta\n");
		expect(lines.totalLines).toBe(3);
		expect(lines.etag.length).toBeGreaterThan(0);
	});

	test("write.lines enforces etag round-trip", async () => {
		const liveSession = assertSession(session);

		const firstEtag = await liveSession.writeLines("etag.txt", "one\n");
		expect(firstEtag.length).toBeGreaterThan(0);
		const firstRead = await liveSession.readLines("etag.txt");
		expect(firstRead.text).toBe("one\n");
		expect(firstRead.etag).toBe(firstEtag);
		const secondEtag = await liveSession.writeLines("etag.txt", "two\n", { ifMatch: firstEtag });
		expect(secondEtag).not.toBe(firstEtag);
		await expect(liveSession.writeLines("etag.txt", "three\n", { ifMatch: firstEtag })).rejects.toBeInstanceOf(
			EtagMismatchError,
		);
	});

	test("GET /sessions/{id}/glob returns expected entries", async () => {
		const liveSession = assertSession(session);

		const glob = await liveSession.glob(["*.txt", "*.db"], { gitignore: false, hidden: false, limit: 20 });
		expect(glob.paths.some(entry => entry.path.endsWith("read-lines.txt"))).toBe(true);
		expect(glob.paths.some(entry => entry.path.endsWith("etag.txt"))).toBe(true);
	});

	test("GET /sessions/{id}/grep streams ndjson GrepRecord values", async () => {
		const liveSession = assertSession(session);

		const grep = await collect(await liveSession.grep("beta", { paths: ["read-lines.txt"], gitignore: false }));
		expect(grep.length).toBeGreaterThan(0);
		expect(grep.some((record: GrepRecord) => record.kind === "match" && record.text.includes("beta"))).toBe(true);
	});

	test("POST /sessions/{id}/bash.exec streams output and ends with exit", async () => {
		const liveSession = assertSession(session);

		const bash = await collect(await liveSession.bashExec({ command: "echo hello", pty: false }));
		expect(bashOutput(bash)).toContain("hello\n");
		expect(bash.at(-1)?.type).toBe("exit");
	});

	test("GET /sessions/{id}/read.db reads a temp sqlite database", async () => {
		const liveSession = assertSession(session);

		await liveSession.writeDb({
			path: "items.db",
			op: "exec",
			sql: "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);",
		});
		await liveSession.writeDb({
			path: "items.db",
			op: "exec",
			sql: "INSERT INTO items (name) VALUES ('alpha');",
		});
		const tables = assertTables(await liveSession.readDb("items.db"));
		expect(tables.tables.some(table => table.name === "items" && table.row_count === 1)).toBe(true);
		const rows = assertRows(await liveSession.readDb("items.db", { table: "items" }));
		expect(rows.rows).toHaveLength(1);
		expect(rows.rows[0]).toMatchObject({ id: 1, name: "alpha" });
	});

	test("DELETE /sessions/{id} returns 204", async () => {
		const liveServer = server;
		const liveClient = client;
		const liveTempDir = tempDir;
		if (liveServer === undefined || liveClient === undefined || liveTempDir === undefined) {
			throw new Error("live server/client/temp dir is not initialized");
		}

		const disposableSession = await liveClient.createSession({ cwd: liveTempDir, env: {} });
		const response = await requestLive(liveServer, `/sessions/${disposableSession.id}`, { method: "DELETE" });
		expect(response.status).toBe(204);
	});

	describe.skip("websocket endpoints", () => {
		// P2-31 covers WS endpoints (LSP/DAP/CDP).
		test("placeholder", () => {});
	});
});
