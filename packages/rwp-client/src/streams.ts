import type { NdjsonWebSocketOptions } from "./types";

export interface JsonWebSocket<TIn, _TOut> {
	ws: WebSocket;
	send(message: TIn): void;
	close(code?: number, reason?: string): void;
}

export async function* ndjsonStream<T>(body: ReadableStream<Uint8Array>): AsyncIterableIterator<T> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	let buffer = "";

	try {
		while (true) {
			const { done, value } = await reader.read();
			if (done) {
				break;
			}
			buffer += decoder.decode(value, { stream: true });
			while (true) {
				const newline = buffer.indexOf("\n");
				if (newline === -1) {
					break;
				}
				const line = buffer.slice(0, newline).trim();
				buffer = buffer.slice(newline + 1);
				if (line.length === 0) {
					continue;
				}
				yield JSON.parse(line) as T;
			}
			if (!body.locked) {
				break;
			}
		}
		buffer += decoder.decode();
		const tail = buffer.trim();
		if (tail.length > 0) {
			yield JSON.parse(tail) as T;
		}
	} catch (error) {
		if (error instanceof DOMException && error.name === "AbortError") {
			return;
		}
		throw error;
	} finally {
		reader.releaseLock();
		if (body.locked) {
			await body.cancel();
		}
	}
}

export async function* ndjsonStreamWithReturn<T, TReturn>(
	body: ReadableStream<Uint8Array>,
	opts: {
		isReturnRecord: (value: unknown) => boolean;
		mapReturn: (value: unknown) => TReturn;
	},
): AsyncGenerator<T, TReturn> {
	const reader = body.getReader();
	const decoder = new TextDecoder();
	let buffer = "";
	let summary: TReturn | undefined;

	try {
		while (true) {
			const { done, value } = await reader.read();
			if (done) {
				break;
			}
			buffer += decoder.decode(value, { stream: true });
			while (true) {
				const newline = buffer.indexOf("\n");
				if (newline === -1) {
					break;
				}
				const line = buffer.slice(0, newline).trim();
				buffer = buffer.slice(newline + 1);
				if (line.length === 0) {
					continue;
				}
				const parsed = JSON.parse(line) as unknown;
				if (opts.isReturnRecord(parsed)) {
					summary = opts.mapReturn(parsed);
					continue;
				}
				yield parsed as T;
			}
			if (!body.locked) {
				break;
			}
		}
		buffer += decoder.decode();
		const tail = buffer.trim();
		if (tail.length > 0) {
			const parsed = JSON.parse(tail) as unknown;
			if (opts.isReturnRecord(parsed)) {
				summary = opts.mapReturn(parsed);
			} else {
				yield parsed as T;
			}
		}
	} catch (error) {
		if (error instanceof DOMException && error.name === "AbortError") {
			return summary as TReturn;
		}
		throw error;
	} finally {
		reader.releaseLock();
		if (body.locked) {
			await body.cancel();
		}
	}
	return summary as TReturn;
}

export function openWebSocket<TIn, TOut>(
	url: string | URL,
	options: NdjsonWebSocketOptions<TOut> = {},
): JsonWebSocket<TIn, TOut> {
	const ws = new WebSocket(url);
	ws.addEventListener("message", event => {
		if (typeof event.data !== "string") {
			return;
		}
		options.onMessage?.(JSON.parse(event.data) as TOut, event as MessageEvent<string>);
	});
	ws.addEventListener("close", event => {
		options.onClose?.(event);
	});
	ws.addEventListener("error", event => {
		options.onError?.(event);
	});
	return {
		ws,
		send(message) {
			ws.send(JSON.stringify(message));
		},
		close(code, reason) {
			ws.close(code, reason);
		},
	};
}
