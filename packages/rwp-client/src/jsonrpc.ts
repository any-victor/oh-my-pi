// Token auth uses ?token= fallback for this cut because openWebSocket does not yet expose Bun upgrade headers.
import { RwpError } from "./errors";
import { type JsonWebSocket, openWebSocket } from "./streams";

type Timer = ReturnType<typeof setTimeout>;

interface JsonRpcRequestMessage {
	jsonrpc: "2.0";
	id: number;
	method: string;
	params: unknown;
}

interface JsonRpcNotificationMessage {
	jsonrpc: "2.0";
	method: string;
	params: unknown;
}

interface JsonRpcResponseError {
	code: number;
	message: string;
	data?: unknown;
}

interface JsonRpcInboundMessage {
	jsonrpc?: "2.0";
	id?: number;
	method?: string;
	params?: unknown;
	result?: unknown;
	error?: JsonRpcResponseError;
}

interface PendingRequest {
	resolve: (value: unknown) => void;
	reject: (error: Error) => void;
	timeout?: Timer;
	signal?: AbortSignal;
	abortHandler?: () => void;
}

interface JsonRpcConnection {
	transport: JsonWebSocket<JsonRpcRequestMessage | JsonRpcNotificationMessage, JsonRpcInboundMessage>;
	opened: Promise<void>;
	closed: Promise<void>;
	resolveClosed: () => void;
	settled: boolean;
}

export interface JsonRpcChannel extends AsyncDisposable {
	request<TResp = unknown>(
		method: string,
		params?: unknown,
		opts?: { signal?: AbortSignal; timeoutMs?: number },
	): Promise<TResp>;
	notify(method: string, params?: unknown): void;
	onNotification(handler: (method: string, params: unknown) => void): () => void;
	close(): Promise<void>;
	reconnect(): Promise<void>;
}

function withToken(url: string | URL, token?: string): URL {
	const next = new URL(url);
	if (token && !next.searchParams.has("token")) {
		next.searchParams.set("token", token);
	}
	return next;
}

function createChannelError(status: number, code: string, message: string, detail?: unknown): RwpError {
	return new RwpError(status, { code, message, detail });
}

function createAbortError(signal?: AbortSignal): Error {
	if (signal?.reason instanceof Error) {
		return signal.reason;
	}
	return createChannelError(499, "aborted", "JSON-RPC request aborted");
}

function createJsonRpcError(error: JsonRpcResponseError): RwpError {
	return new RwpError(error.code, {
		code: String(error.code),
		message: error.message,
		detail: error.data,
	});
}

function delay(ms: number): Promise<void> {
	const { promise, resolve } = Promise.withResolvers<void>();
	setTimeout(resolve, ms);
	return promise;
}

function clearPendingRequest(id: number, pending: PendingRequest, pendingRequests: Map<number, PendingRequest>): void {
	pendingRequests.delete(id);
	if (pending.timeout) {
		clearTimeout(pending.timeout);
	}
	if (pending.signal && pending.abortHandler) {
		pending.signal.removeEventListener("abort", pending.abortHandler);
	}
}

function waitForSocketOpen(socket: WebSocket, signal?: AbortSignal): Promise<void> {
	if (socket.readyState === WebSocket.OPEN) {
		return Promise.resolve();
	}
	if (signal?.aborted) {
		return Promise.reject(createAbortError(signal));
	}
	const deferred = Promise.withResolvers<void>();
	const cleanup = () => {
		socket.removeEventListener("open", onOpen);
		socket.removeEventListener("error", onError);
		socket.removeEventListener("close", onClose);
		signal?.removeEventListener("abort", onAbort);
	};
	const onOpen = () => {
		cleanup();
		deferred.resolve();
	};
	const onError = () => {
		cleanup();
		deferred.reject(createChannelError(502, "ws-error", "WebSocket connection failed"));
	};
	const onClose = () => {
		cleanup();
		deferred.reject(createChannelError(503, "ws-closed", "WebSocket connection closed before opening"));
	};
	const onAbort = () => {
		cleanup();
		deferred.reject(createAbortError(signal));
	};
	socket.addEventListener("open", onOpen, { once: true });
	socket.addEventListener("error", onError, { once: true });
	socket.addEventListener("close", onClose, { once: true });
	signal?.addEventListener("abort", onAbort, { once: true });
	return deferred.promise;
}

export async function openJsonRpcChannel(
	url: string | URL,
	opts: { token?: string; reconnect?: { maxAttempts?: number; backoffMs?: number } } = {},
): Promise<JsonRpcChannel> {
	const reconnectMaxAttempts = opts.reconnect?.maxAttempts ?? 0;
	const reconnectBackoffMs = opts.reconnect?.backoffMs ?? 100;
	const handlers = new Set<(method: string, params: unknown) => void>();
	const pendingRequests = new Map<number, PendingRequest>();
	let nextId = 1;
	let currentConnection: JsonRpcConnection | null = null;
	let connectPromise: Promise<JsonRpcConnection> | null = null;
	let reconnectPromise: Promise<void> | null = null;
	let manuallyClosed = false;
	let suppressReconnect = false;

	const failPendingRequests = (error: Error) => {
		for (const [id, pending] of pendingRequests) {
			clearPendingRequest(id, pending, pendingRequests);
			pending.reject(error);
		}
	};

	const settleConnection = (connection: JsonRpcConnection, error: Error, shouldReconnect: boolean) => {
		if (connection.settled) {
			return;
		}
		connection.settled = true;
		if (currentConnection === connection) {
			currentConnection = null;
		}
		failPendingRequests(error);
		connection.resolveClosed();
		if (shouldReconnect && !manuallyClosed && !suppressReconnect && reconnectMaxAttempts > 0) {
			void scheduleReconnect();
		}
	};

	const openConnection = async (signal?: AbortSignal): Promise<JsonRpcConnection> => {
		if (currentConnection && currentConnection.transport.ws.readyState === WebSocket.OPEN) {
			return currentConnection;
		}
		if (connectPromise) {
			return await connectPromise;
		}
		const deferred = Promise.withResolvers<JsonRpcConnection>();
		connectPromise = deferred.promise;
		const closedDeferred = Promise.withResolvers<void>();
		const transport = openWebSocket<JsonRpcRequestMessage | JsonRpcNotificationMessage, JsonRpcInboundMessage>(
			withToken(url, opts.token),
			{
				onMessage(message) {
					if (typeof message !== "object" || message === null) {
						return;
					}
					if (typeof message.id === "number") {
						const pending = pendingRequests.get(message.id);
						if (!pending) {
							return;
						}
						clearPendingRequest(message.id, pending, pendingRequests);
						if (message.error) {
							pending.reject(createJsonRpcError(message.error));
							return;
						}
						pending.resolve(message.result);
						return;
					}
					if (typeof message.method === "string") {
						for (const handler of handlers) {
							handler(message.method, message.params);
						}
					}
				},
				onClose(event) {
					const reason = event.reason || "WebSocket connection closed";
					settleConnection(connection, createChannelError(503, "ws-closed", reason), true);
				},
				onError() {
					settleConnection(connection, createChannelError(502, "ws-error", "WebSocket channel error"), true);
				},
			},
		);
		const connection: JsonRpcConnection = {
			transport,
			opened: waitForSocketOpen(transport.ws, signal),
			closed: closedDeferred.promise,
			resolveClosed: closedDeferred.resolve,
			settled: false,
		};
		connection.opened
			.then(() => {
				currentConnection = connection;
				manuallyClosed = false;
				deferred.resolve(connection);
			})
			.catch(error => {
				settleConnection(
					connection,
					error instanceof Error ? error : createChannelError(502, "ws-error", "WebSocket connection failed"),
					false,
				);
				deferred.reject(
					error instanceof Error ? error : createChannelError(502, "ws-error", "WebSocket connection failed"),
				);
			})
			.finally(() => {
				if (connectPromise === deferred.promise) {
					connectPromise = null;
				}
			});
		return await deferred.promise;
	};

	const scheduleReconnect = async (): Promise<void> => {
		if (reconnectPromise) {
			return await reconnectPromise;
		}
		const deferred = Promise.withResolvers<void>();
		reconnectPromise = deferred.promise;
		void (async () => {
			let lastError: Error | null = null;
			for (let attempt = 1; attempt <= reconnectMaxAttempts; attempt += 1) {
				if (manuallyClosed) {
					deferred.resolve();
					return;
				}
				if (attempt > 1 && reconnectBackoffMs > 0) {
					await delay(reconnectBackoffMs);
				}
				try {
					await openConnection();
					deferred.resolve();
					return;
				} catch (error) {
					lastError =
						error instanceof Error ? error : createChannelError(502, "ws-error", "WebSocket connection failed");
				}
			}
			if (lastError) {
				deferred.reject(lastError);
				return;
			}
			deferred.resolve();
		})().finally(() => {
			reconnectPromise = null;
		});
		return await deferred.promise;
	};

	const getConnection = async (signal?: AbortSignal): Promise<JsonRpcConnection> => {
		if (manuallyClosed) {
			throw createChannelError(499, "channel-closed", "JSON-RPC channel is closed; call reconnect() first");
		}
		if (currentConnection && currentConnection.transport.ws.readyState === WebSocket.OPEN) {
			return currentConnection;
		}
		if (reconnectPromise) {
			try {
				await reconnectPromise;
			} catch {
				// Fall through to a fresh connect attempt.
			}
			if (currentConnection && currentConnection.transport.ws.readyState === WebSocket.OPEN) {
				return currentConnection;
			}
		}
		return await openConnection(signal);
	};

	const closeCurrentConnection = async (markManual: boolean): Promise<void> => {
		if (markManual) {
			manuallyClosed = true;
		}
		const connection = currentConnection;
		if (!connection) {
			return;
		}
		suppressReconnect = true;
		currentConnection = null;
		if (
			connection.transport.ws.readyState === WebSocket.CONNECTING ||
			connection.transport.ws.readyState === WebSocket.OPEN
		) {
			connection.transport.close();
		} else {
			settleConnection(connection, createChannelError(503, "ws-closed", "WebSocket connection closed"), false);
		}
		await connection.closed;
		suppressReconnect = false;
	};

	await openConnection();

	return {
		async request<TResp = unknown>(
			method: string,
			params?: unknown,
			requestOptions?: { signal?: AbortSignal; timeoutMs?: number },
		): Promise<TResp> {
			if (requestOptions?.signal?.aborted) {
				throw createAbortError(requestOptions.signal);
			}
			const connection = await getConnection(requestOptions?.signal);
			const id = nextId;
			nextId += 1;
			const deferred = Promise.withResolvers<TResp>();
			const timeoutMs = requestOptions?.timeoutMs ?? 30000;
			const abortHandler = () => {
				const pending = pendingRequests.get(id);
				if (!pending) {
					return;
				}
				clearPendingRequest(id, pending, pendingRequests);
				pending.reject(createAbortError(requestOptions?.signal));
			};
			const timeout = setTimeout(() => {
				const pending = pendingRequests.get(id);
				if (!pending) {
					return;
				}
				clearPendingRequest(id, pending, pendingRequests);
				pending.reject(createChannelError(408, "timeout", `JSON-RPC request timed out after ${timeoutMs}ms`));
			}, timeoutMs);
			pendingRequests.set(id, {
				resolve: value => deferred.resolve(value as TResp),
				reject: deferred.reject,
				timeout,
				signal: requestOptions?.signal,
				abortHandler,
			});
			requestOptions?.signal?.addEventListener("abort", abortHandler, { once: true });
			try {
				connection.transport.send({ jsonrpc: "2.0", id, method, params: params ?? null });
			} catch (error) {
				const pending = pendingRequests.get(id);
				if (pending) {
					clearPendingRequest(id, pending, pendingRequests);
					pending.reject(
						error instanceof Error
							? error
							: createChannelError(502, "ws-error", "Failed to send JSON-RPC request"),
					);
				}
			}
			return await deferred.promise;
		},
		notify(method: string, params?: unknown): void {
			if (manuallyClosed || !currentConnection || currentConnection.transport.ws.readyState !== WebSocket.OPEN) {
				throw createChannelError(499, "channel-closed", "JSON-RPC channel is not connected");
			}
			currentConnection.transport.send({ jsonrpc: "2.0", method, params: params ?? null });
		},
		onNotification(handler: (method: string, params: unknown) => void): () => void {
			handlers.add(handler);
			return () => {
				handlers.delete(handler);
			};
		},
		async close(): Promise<void> {
			await closeCurrentConnection(true);
		},
		async reconnect(): Promise<void> {
			await closeCurrentConnection(false);
			manuallyClosed = false;
			await openConnection();
		},
		async [Symbol.asyncDispose](): Promise<void> {
			await closeCurrentConnection(true);
		},
	};
}
