import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import type { Server, ServerWebSocket } from "bun";
import { openJsonRpcChannel, RwpClient, RwpError } from "../src";

interface SocketData {
	path: string;
	token: string | null;
}

interface JsonRpcRequestMessage {
	jsonrpc: "2.0";
	id?: number;
	method?: string;
	params?: unknown;
}

let server: Server<SocketData>;
let baseUrl: string;
let activeSocket: ServerWebSocket<SocketData> | null = null;
let onSocketOpen: (socket: ServerWebSocket<SocketData>) => void = () => {};
let onSocketMessage: (socket: ServerWebSocket<SocketData>, message: JsonRpcRequestMessage) => void = () => {};

function sendJson(socket: ServerWebSocket<SocketData>, payload: unknown): void {
	socket.send(JSON.stringify(payload));
}

beforeAll(() => {
	server = Bun.serve<SocketData>({
		port: 0,
		fetch(request, serverInstance) {
			const url = new URL(request.url);
			if (serverInstance.upgrade(request, { data: { path: url.pathname, token: url.searchParams.get("token") } })) {
				return;
			}
			return new Response("upgrade failed", { status: 500 });
		},
		websocket: {
			open(socket) {
				activeSocket = socket;
				onSocketOpen(socket);
			},
			message(socket, message) {
				if (typeof message !== "string") {
					return;
				}
				onSocketMessage(socket, JSON.parse(message) as JsonRpcRequestMessage);
			},
			close(socket) {
				if (activeSocket === socket) {
					activeSocket = null;
				}
			},
		},
		development: false,
	});
	baseUrl = `http://127.0.0.1:${server.port}`;
});

beforeEach(() => {
	onSocketOpen = () => {};
	onSocketMessage = () => {};
	activeSocket?.close();
	activeSocket = null;
});

afterAll(() => {
	activeSocket?.close();
	server.stop(true);
});

describe("openJsonRpcChannel", () => {
	test("correlates request and response ids monotonically", async () => {
		const seenIds: number[] = [];
		onSocketMessage = (socket, message) => {
			seenIds.push(message.id ?? 0);
			if (message.id === 1) {
				setTimeout(() => {
					sendJson(socket, { jsonrpc: "2.0", id: 1, result: { value: "first" } });
				}, 10);
			}
			if (message.id === 2) {
				sendJson(socket, { jsonrpc: "2.0", id: 2, result: { value: "second" } });
			}
		};
		const channel = await openJsonRpcChannel(new URL("/rpc", baseUrl));
		const firstPromise = channel.request<{ value: string }>("first", { n: 1 });
		const secondPromise = channel.request<{ value: string }>("second", { n: 2 });
		expect(await secondPromise).toEqual({ value: "second" });
		expect(await firstPromise).toEqual({ value: "first" });
		expect(seenIds).toEqual([1, 2]);
		await channel.close();
	});

	test("dispatches notifications to multiple handlers and supports handler removal", async () => {
		const channel = await openJsonRpcChannel(new URL("/rpc", baseUrl));
		const firstCalls: Array<{ method: string; params: unknown }> = [];
		const secondCalls: Array<{ method: string; params: unknown }> = [];
		const sawAlpha = Promise.withResolvers<void>();
		const sawBeta = Promise.withResolvers<void>();
		const unsubscribeFirst = channel.onNotification((method, params) => {
			firstCalls.push({ method, params });
			if (method === "alpha") {
				sawAlpha.resolve();
			}
		});
		channel.onNotification((method, params) => {
			secondCalls.push({ method, params });
			if (method === "beta") {
				sawBeta.resolve();
			}
		});

		expect(activeSocket).not.toBeNull();
		const socket = activeSocket;
		if (!socket) {
			throw new Error("expected active websocket");
		}
		sendJson(socket, { jsonrpc: "2.0", method: "alpha", params: { x: 1 } });
		await sawAlpha.promise;
		unsubscribeFirst();
		sendJson(socket, { jsonrpc: "2.0", method: "beta", params: { x: 2 } });
		await sawBeta.promise;

		expect(firstCalls).toEqual([{ method: "alpha", params: { x: 1 } }]);
		expect(secondCalls).toEqual([
			{ method: "alpha", params: { x: 1 } },
			{ method: "beta", params: { x: 2 } },
		]);
		await channel.close();
	});

	test("rejects matching promise on error response", async () => {
		onSocketMessage = (socket, message) => {
			sendJson(socket, {
				jsonrpc: "2.0",
				id: message.id,
				error: { code: -32001, message: "bad request" },
			});
		};
		const channel = await openJsonRpcChannel(new URL("/rpc", baseUrl));
		const request = channel.request("explode");
		await expect(request).rejects.toMatchObject({
			name: "RwpError",
			code: "-32001",
			message: "bad request",
		});
		await channel.close();
	});

	test("rejects all in-flight requests when the socket closes", async () => {
		const seenRequests = Promise.withResolvers<void>();
		let requestCount = 0;
		onSocketMessage = socket => {
			requestCount += 1;
			if (requestCount === 2) {
				seenRequests.resolve();
				socket.close(1011, "server gone");
			}
		};
		const channel = await openJsonRpcChannel(new URL("/rpc", baseUrl));
		const first = channel.request("one").then(
			() => null,
			error => error,
		);
		const second = channel.request("two").then(
			() => null,
			error => error,
		);
		await seenRequests.promise;
		await expect(first).resolves.toBeInstanceOf(RwpError);
		await expect(second).resolves.toBeInstanceOf(RwpError);
		await channel.close();
	});

	test("rejects on timeout when no response arrives", async () => {
		onSocketMessage = () => {};
		const channel = await openJsonRpcChannel(new URL("/rpc", baseUrl));
		const startedAt = Date.now();
		await expect(channel.request("slow", undefined, { timeoutMs: 25 })).rejects.toMatchObject({
			name: "RwpError",
			code: "timeout",
		});
		expect(Date.now() - startedAt).toBeGreaterThanOrEqual(20);
		await channel.close();
	});

	test("opens typed LSP channels through the client websocket URL builder", async () => {
		onSocketMessage = (socket, message) => {
			sendJson(socket, { jsonrpc: "2.0", id: message.id, result: { ok: true } });
		};
		const client = new RwpClient({ baseUrl, token: "secret-token" });
		const channel = await client.openLspChannel("main");
		expect(activeSocket?.data.path).toBe("/lsp/main");
		expect(activeSocket?.data.token).toBe("secret-token");
		expect(await channel.request<{ ok: boolean }>("initialize")).toEqual({ ok: true });
		await channel.close();
	});
});
