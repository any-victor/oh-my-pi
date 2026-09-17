import * as path from "node:path";
import {
	type CdpHandleConfig,
	type ErrorBody,
	NotFoundError,
	RwpClient,
	type RwpError,
	RwpSession,
	toRwpError,
} from "@oh-my-pi/rwp-client";
import { openWebSocket } from "@oh-my-pi/rwp-client/streams";
import { notebookToEditableText } from "@oh-my-pi/pi-natives";
import { convertBufferWithMarkit } from "../utils/markit";
import {
	type Backend,
	BackendEditPipeline,
	type BrowserBackend,
	type DapBackend,
	type EditBackend,
	type FsBackend,
	type JsonRpcChannel,
	type KernelBackend,
	type LspBackend,
	type ShellBackend,
	type SqliteBackend,
} from "./backend";
import type * as T from "./types";

function detectEol(text: string): "LF" | "CRLF" | "CR" {
	let crlf = 0;
	let lf = 0;
	let cr = 0;
	for (let index = 0; index < text.length; index += 1) {
		const ch = text[index];
		if (ch === "\r") {
			if (text[index + 1] === "\n") {
				crlf += 1;
				index += 1;
			} else {
				cr += 1;
			}
			continue;
		}
		if (ch === "\n") lf += 1;
	}
	if (crlf >= lf && crlf >= cr && crlf > 0) return "CRLF";
	if (lf >= cr && lf > 0) return "LF";
	if (cr > 0) return "CR";
	return "LF";
}

function normalizeNewlines(text: string): string {
	return text.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
}

function toRangeString(range: { start: number; end: number } | undefined): string | undefined {
	return range ? `${range.start}-${range.end}` : undefined;
}
function stripHeaderQuotes(value: string | null): string | null {
	return value ? value.replace(/^"|"$/g, "") : null;
}

function normalizeScopedPath(input: string): string {
	const normalized = input.replaceAll("\\", "/");
	const normalizedPath = path.posix.normalize(normalized);
	return normalizedPath === "." ? "." : normalizedPath.replace(/^\.\/+/, "").replace(/\/+$/, "");
}

function isWithinScopedPath(entryPath: string, scopedPath: string): boolean {
	if (scopedPath === "" || scopedPath === ".") return true;
	const normalizedEntryPath = normalizeScopedPath(entryPath);
	const normalizedScopedPath = normalizeScopedPath(scopedPath);
	return normalizedEntryPath === normalizedScopedPath || normalizedEntryPath.startsWith(`${normalizedScopedPath}/`);
}

function toLspDiagnostics(
	diagnostics: import("@oh-my-pi/rwp-client").LspGetResponse["diagnostics"],
): Map<string, import("../lsp/types").Diagnostic[]> | undefined {
	return diagnostics
		? new Map(Object.entries(diagnostics).map(([uri, published]) => [uri, [...published]]))
		: undefined;
}

type RemoteGlobPathEntry = {
	path: string;
	mtime: number;
	size: number;
	type?: string;
	kind?: string;
};

function depthOfRelativePath(relativePath: string): number {
	if (relativePath === "" || relativePath === ".") return 0;
	return relativePath.split("/").length - 1;
}

function coerceGlobEntryType(type: string | undefined): T.GlobResult["entries"][number]["type"] | null {
	switch (type) {
		case "file":
		case "dir":
		case "symlink":
		case "other":
			return type;
		default:
			return null;
	}
}

interface ArchiveSnapshotOpenResponse {
	snapshot_id: string;
	format: T.ArchiveSnapshot["format"];
}

function toStableBashEvent(event: {
	type: string;
	data?: string;
	code?: number | null;
	cancelled?: boolean;
	timed_out?: boolean;
	minimizer?: {
		minimized: boolean;
		original_lines: number;
		minimized_lines: number;
		omitted_lines: number;
		truncated: boolean;
		raw_artifact?: { kind: "path"; path: string } | { kind: "bytes"; bytes: string };
	};
}): T.BashEvent {
	if (event.type === "output") {
		return { type: "output", text: event.data ?? "" };
	}
	if (event.type === "stdout") {
		return { type: "stdout", data: event.data ?? "" };
	}
	if (event.type === "stderr") {
		return { type: "stderr", data: event.data ?? "" };
	}
	return {
		type: "exit",
		exitCode: event.code ?? null,
		signaled: event.cancelled === true || event.timed_out === true,
		cancelled: event.cancelled,
		timedOut: event.timed_out,
		minimizer: event.minimizer
			? {
					minimized: event.minimizer.minimized,
					originalLines: event.minimizer.original_lines,
					minimizedLines: event.minimizer.minimized_lines,
					omittedLines: event.minimizer.omitted_lines,
					truncated: event.minimizer.truncated,
					rawArtifact:
						event.minimizer.raw_artifact?.kind === "bytes"
							? {
									kind: "bytes",
									bytes: Uint8Array.from(Buffer.from(event.minimizer.raw_artifact.bytes, "base64")),
								}
							: event.minimizer.raw_artifact,
				}
			: undefined,
	};
}

function mapKernelState(state: string): T.KernelStatus["state"] {
	return state === "starting" ? "starting" : "ready";
}

function mapGrepAstHit(hit: {
	path: string;
	line: number;
	column: number;
	end_line: number;
	end_column: number;
	text: string;
}): T.GrepAstHit {
	return {
		file: hit.path,
		range: { start: hit.line, end: hit.end_line },
		column: hit.column,
		endColumn: hit.end_column,
		matched: hit.text,
		meta: {},
	};
}

function getAstEditOps(req: T.EditAstRequest): NonNullable<T.EditAstRequest["ops"]> {
	const ops = req.ops ?? req.rules;
	if (!ops || ops.length === 0) {
		throw new Error("editAst requires at least one rewrite op");
	}
	return ops;
}
function mapAstEditFileChange(change: {
	path: string;
	replacements: number;
	before_lines: string[];
	after_lines: string[];
	hunks: Array<{ before_start: number; before_lines: string[]; after_lines: string[] }>;
}): T.AstEditFileChange {
	return {
		path: change.path,
		replacements: change.replacements,
		beforeLines: change.before_lines,
		afterLines: change.after_lines,
		hunks: change.hunks.map(hunk => ({
			beforeStart: hunk.before_start,
			beforeLines: hunk.before_lines,
			afterLines: hunk.after_lines,
		})),
	};
}

function mapAstEditResult(result: {
	changes: T.AstEditResult["changes"];
	file_changes?: Array<{
		path: string;
		replacements: number;
		before_lines: string[];
		after_lines: string[];
		hunks: Array<{ before_start: number; before_lines: string[]; after_lines: string[] }>;
	}>;
	parse_errors?: Array<{ file?: string | null; message: string }>;
	files_searched?: number;
	limit_reached?: boolean;
	written?: boolean;
	truncated?: boolean;
	exceeded_limit?: boolean;
}): T.AstEditResult {
	return {
		changes: result.changes,
		...(result.file_changes ? { fileChanges: result.file_changes.map(mapAstEditFileChange) } : {}),
		parseErrors: (result.parse_errors ?? []).map(parseError => ({
			...parseError,
			file: parseError.file ?? undefined,
		})),
		filesSearched: result.files_searched ?? 0,
		limitReached: result.limit_reached ?? false,
		written: result.written ?? false,
		truncated: result.truncated ?? false,
		exceededLimit: result.exceeded_limit ?? false,
	};
}

async function toClientError(response: Response): Promise<RwpError> {
	let body: ErrorBody | null = null;
	try {
		body = (await response.json()) as ErrorBody;
	} catch {}
	return toRwpError(response.status, body ?? { code: "unknown", message: response.statusText });
}

function toCdpHandleConfig(config: T.BrowserConfig): CdpHandleConfig {
	const { signal: _signal, ...handleConfig } = config;
	return handleConfig;
}

async function waitForWebSocketOpen(ws: WebSocket, signal?: AbortSignal): Promise<void> {
	if (ws.readyState === WebSocket.OPEN) return;
	if (signal?.aborted) throw signal.reason;
	await new Promise<void>((resolve, reject) => {
		const cleanup = () => {
			ws.removeEventListener("open", onOpen);
			ws.removeEventListener("error", onError);
			ws.removeEventListener("close", onClose);
			signal?.removeEventListener("abort", onAbort);
		};
		const onOpen = () => {
			cleanup();
			resolve();
		};
		const onError = () => {
			cleanup();
			reject(new Error("WebSocket connection failed"));
		};
		const onClose = () => {
			cleanup();
			reject(new Error("WebSocket connection closed before opening"));
		};
		const onAbort = () => {
			cleanup();
			reject(signal?.reason);
		};
		ws.addEventListener("open", onOpen, { once: true });
		ws.addEventListener("error", onError, { once: true });
		ws.addEventListener("close", onClose, { once: true });
		signal?.addEventListener("abort", onAbort, { once: true });
	});
}

type JsonRpcResponseEnvelope = {
	id?: number;
	result?: unknown;
	error?: { message?: string };
};

type JsonRpcNotificationEnvelope = {
	method?: string;
	params?: unknown;
};

type ReverseRequestFrame = {
	type?: string;
	id?: number;
	method?: string;
	params?: unknown;
};

function createJsonRpcChannel(
	url: URL,
	signal?: AbortSignal,
	opts?: { allowReverseRequests?: boolean },
): Promise<JsonRpcChannel> {
	const handlers = new Set<(method: string, params: unknown) => void>();
	const reverseRequestHandlers = new Map<string, (params: unknown) => Promise<unknown>>();
	const pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
	let nextId = 0;
	const socket = openWebSocket<unknown, unknown>(url, {
		onMessage(message) {
			if (typeof message !== "object" || message === null) {
				return;
			}
			const sendReverseResponse = (id: number, body: { result?: unknown; error?: { message: string } }): void => {
				socket.send({
					type: "reverseResponse",
					id,
					...(body.error ? { error: body.error } : { result: body.result ?? null }),
				});
			};
			const dispatchReverseRequest = (id: number, method: string, params: unknown): void => {
				const handler = reverseRequestHandlers.get(method);
				if (!handler) {
					sendReverseResponse(id, {
						error: { message: `No reverse request handler registered for ${method}` },
					});
					return;
				}
				void handler(params)
					.then(result => {
						sendReverseResponse(id, { result });
					})
					.catch(error => {
						sendReverseResponse(id, {
							error: {
								message: error instanceof Error ? error.message : "Reverse request failed",
							},
						});
					});
			};
			if (opts?.allowReverseRequests) {
				const reverseFrame = message as ReverseRequestFrame;
				if (
					reverseFrame.type === "reverseRequest" &&
					typeof reverseFrame.id === "number" &&
					typeof reverseFrame.method === "string"
				) {
					dispatchReverseRequest(reverseFrame.id, reverseFrame.method, reverseFrame.params);
					return;
				}
			}
			const responseEnvelope = message as JsonRpcResponseEnvelope;
			if (
				typeof responseEnvelope.id === "number" &&
				!(
					opts?.allowReverseRequests &&
					typeof (message as ReverseRequestFrame).method === "string" &&
					(message as ReverseRequestFrame).type !== "reverseResponse"
				)
			) {
				const waiter = pending.get(responseEnvelope.id);
				if (!waiter) return;
				pending.delete(responseEnvelope.id);
				if (responseEnvelope.error) {
					waiter.reject(new Error(responseEnvelope.error.message ?? "JSON-RPC request failed"));
					return;
				}
				waiter.resolve(responseEnvelope.result);
				return;
			}
			const notificationEnvelope = message as JsonRpcNotificationEnvelope;
			if (typeof notificationEnvelope.method === "string") {
				if (opts?.allowReverseRequests && typeof (message as ReverseRequestFrame).id === "number") {
					dispatchReverseRequest(
						(message as ReverseRequestFrame).id as number,
						notificationEnvelope.method,
						notificationEnvelope.params,
					);
					return;
				}
				for (const handler of handlers) {
					handler(notificationEnvelope.method, notificationEnvelope.params);
				}
			}
		},
		onClose() {
			for (const waiter of pending.values()) {
				waiter.reject(new Error("JSON-RPC channel closed"));
			}
			pending.clear();
			reverseRequestHandlers.clear();
		},
	});
	const close = async (): Promise<void> => {
		socket.close();
	};
	return waitForWebSocketOpen(socket.ws, signal).then(() => ({
		async request<TResp = unknown>(
			method: string,
			params?: unknown,
			opts?: { signal?: AbortSignal; timeoutMs?: number },
		): Promise<TResp> {
			const id = ++nextId;
			return await new Promise<TResp>((resolve, reject) => {
				const cleanup = (abortHandler: (() => void) | undefined, timeout: Timer | undefined) => {
					if (timeout) clearTimeout(timeout);
					if (abortHandler) opts?.signal?.removeEventListener("abort", abortHandler);
				};
				let abortHandler: (() => void) | undefined;
				const timeout =
					typeof opts?.timeoutMs === "number"
						? setTimeout(() => {
								pending.delete(id);
								rejectPending(new Error(`JSON-RPC request ${method} timed out after ${opts.timeoutMs}ms`));
							}, opts.timeoutMs)
						: undefined;
				const resolvePending = (value: unknown) => {
					cleanup(abortHandler, timeout);
					resolve(value as TResp);
				};
				const rejectPending = (error: Error) => {
					cleanup(abortHandler, timeout);
					reject(error);
				};
				abortHandler = () => {
					const waiter = pending.get(id);
					if (!waiter) return;
					pending.delete(id);
					rejectPending(opts?.signal?.reason instanceof Error ? opts.signal.reason : new Error("Request aborted"));
				};
				pending.set(id, {
					resolve: resolvePending,
					reject: rejectPending,
				});
				opts?.signal?.addEventListener("abort", abortHandler, { once: true });
				try {
					socket.send({ jsonrpc: "2.0", id, method, params: params ?? null });
				} catch (error) {
					pending.delete(id);
					rejectPending(error instanceof Error ? error : new Error(String(error)));
				}
			});
		},
		notify(method: string, params?: unknown): void {
			socket.send({ jsonrpc: "2.0", method, params: params ?? null });
		},
		onNotification(handler: (method: string, params: unknown) => void): () => void {
			handlers.add(handler);
			return () => {
				handlers.delete(handler);
			};
		},
		setReverseRequestHandler(method: string, handler: (params: unknown) => Promise<unknown>): () => void {
			reverseRequestHandlers.set(method, handler);
			return () => {
				if (reverseRequestHandlers.get(method) === handler) {
					reverseRequestHandlers.delete(method);
				}
			};
		},
		close,
		async [Symbol.asyncDispose](): Promise<void> {
			await close();
		},
	}));
}

class RemoteFsBackend implements FsBackend {
	constructor(
		private readonly session: () => Promise<RwpSession>,
		private readonly authToken?: string,
	) {}

	async readLines(filePath: string, opts?: T.ReadLinesOptions): Promise<T.ReadLinesResult> {
		const response = await (await this.session()).readLines(filePath, {
			range: toRangeString(opts?.range),
			maxLines: opts?.maxLines,
			maxBytes: opts?.maxBytes,
			signal: opts?.signal,
		});
		const bom = response.text.startsWith("\uFEFF");
		const normalized = normalizeNewlines(bom ? response.text.slice(1) : response.text);
		return {
			lines: normalized.split("\n"),
			startLine: opts?.range?.start ?? 1,
			etag: response.etag,
			eol: detectEol(response.text),
			bom,
			truncated: response.truncated ?? false,
			totalLines: response.totalLines,
		};
	}

	async readBlob(filePath: string, opts?: T.ReadBlobOptions): Promise<T.ReadBlobResult> {
		if (opts?.sizeOnly) {
			const url = await this.#sessionUrl("read.blob");
			url.searchParams.set("path", filePath);
			url.searchParams.set("size_only", "1");
			const response = await fetch(url, {
				signal: opts?.signal,
				headers: this.#authHeaders(),
			});
			if (!response.ok) throw await toClientError(response);
			const body = (await response.json()) as {
				size: number;
				etag: string | null;
				content_type?: string | null;
			};
			return {
				bytes: new Uint8Array(0),
				size: body.size,
				etag: body.etag ?? null,
				contentType: body.content_type ?? undefined,
			};
		}

		const response = await (await this.session()).readBlob(filePath, {
			range: opts?.range ? `bytes=${opts.range.start}-${opts.range.end}` : undefined,
			signal: opts?.signal,
		});
		return {
			bytes: response.bytes,
			size: response.bytes.byteLength,
			etag: response.etag ?? null,
			contentType: response.contentType,
		};
	}

	async readAst(filePath: string, opts?: T.ReadAstOptions): Promise<T.AstSummary> {
		return await (await this.session()).readAst(filePath, {
			language: opts?.language,
			range: toRangeString(opts?.range),
			minBodyLines: opts?.minBodyLines,
			minCommentLines: opts?.minCommentLines,
			signal: opts?.signal,
		});
	}

	async imageMeta(filePath: string, opts?: { signal?: AbortSignal }): Promise<T.ImageMetadata | null> {
		const metadata = await (await this.session()).imageMeta(filePath, { signal: opts?.signal });
		if (!metadata) return null;
		return {
			mimeType: metadata.mime_type as T.ImageMetadata["mimeType"],
			width: metadata.width,
			height: metadata.height,
			channels: metadata.channels,
			hasAlpha: metadata.has_alpha,
		} as T.ImageMetadata;
	}

	async notebookText(filePath: string, displayPath: string, opts?: { signal?: AbortSignal }): Promise<string> {
		const blob = await this.readBlob(filePath, { signal: opts?.signal });
		return notebookToEditableText(new TextDecoder().decode(blob.bytes), displayPath);
	}

	async markitConvert(
		filePath: string,
		extension: string,
		opts?: { signal?: AbortSignal },
	): Promise<T.MarkitConversionResult> {
		const blob = await this.readBlob(filePath, { signal: opts?.signal });
		return await convertBufferWithMarkit(blob.bytes, extension, opts?.signal);
	}

	async listWorkspace(opts: T.ListWorkspaceOptions): Promise<T.ListWorkspaceResult> {
		const result = await (await this.session()).listWorkspace({
			path: opts.path,
			maxDepth: opts.maxDepth,
			hidden: opts.hidden,
			gitignore: opts.gitignore,
			collectAgentsMd: opts.collectAgentsMd,
			timeoutMs: opts.timeoutMs,
			signal: opts.signal,
		});
		return {
			entries: result.entries.map(entry => ({
				path: entry.path,
				fileType: entry.file_type,
				mtime: entry.mtime,
				size: entry.size,
			})),
			agentsMdFiles: result.agents_md_files,
			truncated: result.truncated,
		};
	}

	async writeLines(filePath: string, text: string, opts: T.WriteOptions): Promise<T.WriteResult> {
		const etag = await (await this.session()).writeLines(filePath, text, {
			ifMatch: opts.ifMatch === "*" ? undefined : (opts.ifMatch ?? undefined),
			signal: opts.signal,
		});
		return { etag, written: new TextEncoder().encode(text).byteLength };
	}

	async writeBlob(filePath: string, bytes: Uint8Array, opts: T.WriteOptions): Promise<T.WriteResult> {
		const etag = await (await this.session()).writeBlob(filePath, bytes, {
			ifMatch: opts.ifMatch === "*" ? undefined : (opts.ifMatch ?? undefined),
			signal: opts.signal,
		});
		return { etag, written: bytes.byteLength };
	}

	async stat(filePath: string, opts?: { signal?: AbortSignal; followSymlinks?: boolean }): Promise<T.StatResult> {
		const url = await this.#sessionUrl("stat");
		url.searchParams.set("path", filePath);
		if (opts?.followSymlinks) url.searchParams.set("follow_symlinks", "true");
		const response = await fetch(url, { signal: opts?.signal, headers: this.#authHeaders() });
		if (!response.ok) throw await toClientError(response);
		const body = (await response.json()) as {
			exists: boolean;
			kind: T.StatResult["kind"];
			size: number;
			mtime_ms: number;
			link_kind?: "symlink";
			etag: string | null;
		};
		return {
			exists: body.exists,
			kind: body.kind,
			size: body.size,
			mtimeMs: body.mtime_ms,
			linkKind: body.link_kind,
			etag: body.etag,
		};
	}

	async exists(filePath: string, opts?: { signal?: AbortSignal }): Promise<boolean> {
		const url = await this.#sessionUrl("exists");
		url.searchParams.set("path", filePath);
		const response = await fetch(url, { signal: opts?.signal, headers: this.#authHeaders() });
		if (response.status === 204) return true;
		if (response.status === 404) return false;
		throw await toClientError(response);
	}

	async openArchive(filePath: string, opts?: { signal?: AbortSignal }): Promise<T.ArchiveSnapshot> {
		const openUrl = await this.#sessionUrl("archive.open");
		openUrl.searchParams.set("path", filePath);
		const session = await this.session();
		const baseUrl = session.client.baseUrl;
		const authHeaders = this.#authHeaders();
		const openResponse = await fetch(openUrl, {
			method: "POST",
			signal: opts?.signal,
			headers: authHeaders,
		});
		if (!openResponse.ok) throw await toClientError(openResponse);
		const body = (await openResponse.json()) as ArchiveSnapshotOpenResponse;
		let closed = false;
		const close = async (): Promise<void> => {
			if (closed) return;
			closed = true;
			const closeUrl = new URL(`/archive/${body.snapshot_id}`, baseUrl);
			const response = await fetch(closeUrl, {
				method: "DELETE",
				headers: authHeaders,
			});
			if (!response.ok && response.status !== 404) {
				throw await toClientError(response);
			}
		};
		return {
			format: body.format,
			async entries(snapshotOpts?: { signal?: AbortSignal }): Promise<T.ArchiveEntry[]> {
				if (closed) throw new Error("Archive snapshot is closed");
				const entriesUrl = new URL(`/archive/${body.snapshot_id}/entries`, baseUrl);
				const response = await fetch(entriesUrl, {
					signal: snapshotOpts?.signal,
					headers: authHeaders,
				});
				if (!response.ok) throw await toClientError(response);
				const entriesBody = (await response.json()) as T.ArchiveEntriesResult;
				return entriesBody.entries;
			},
			async readEntry(name: string, snapshotOpts?: { signal?: AbortSignal }): Promise<Uint8Array> {
				if (closed) throw new Error("Archive snapshot is closed");
				const entryUrl = new URL(`/archive/${body.snapshot_id}/entry`, baseUrl);
				entryUrl.searchParams.set("path", name);
				const response = await fetch(entryUrl, {
					signal: snapshotOpts?.signal,
					headers: authHeaders,
				});
				if (!response.ok) throw await toClientError(response);
				return new Uint8Array(await response.arrayBuffer());
			},
			close,
			async [Symbol.asyncDispose](): Promise<void> {
				await close();
			},
		};
	}

	async delete(filePath: string, opts?: { signal?: AbortSignal }): Promise<void> {
		await (await this.session()).deleteFile(filePath, { signal: opts?.signal });
	}

	async mkdir(filePath: string, opts?: { recursive?: boolean; signal?: AbortSignal }): Promise<void> {
		await (await this.session()).mkdir(filePath, {
			recursive: opts?.recursive,
			signal: opts?.signal,
		});
	}

	async rename(from: string, to: string, opts?: { overwrite?: boolean; signal?: AbortSignal }): Promise<void> {
		await (await this.session()).rename(from, to, {
			overwrite: opts?.overwrite,
			signal: opts?.signal,
		});
	}

	async archiveEntries(filePath: string, opts?: T.ArchiveEntriesOptions): Promise<T.ArchiveEntriesResult> {
		const url = await this.#sessionUrl("archive.entries");
		url.searchParams.set("path", filePath);
		if (opts?.prefix) url.searchParams.set("prefix", opts.prefix);
		if (opts?.limit !== undefined) url.searchParams.set("limit", String(opts.limit));
		const response = await fetch(url, { signal: opts?.signal, headers: this.#authHeaders() });
		if (!response.ok) throw await toClientError(response);
		return (await response.json()) as T.ArchiveEntriesResult;
	}

	async archiveReadEntry(filePath: string, entry: string, opts?: { signal?: AbortSignal }): Promise<T.ReadBlobResult> {
		const url = await this.#sessionUrl("archive.read");
		url.searchParams.set("path", filePath);
		url.searchParams.set("entry", entry);
		const response = await fetch(url, { signal: opts?.signal, headers: this.#authHeaders() });
		if (!response.ok) throw await toClientError(response);
		const bytes = new Uint8Array(await response.arrayBuffer());
		return {
			bytes,
			size: bytes.byteLength,
			etag: stripHeaderQuotes(response.headers.get("etag")),
			contentType: response.headers.get("content-type") ?? undefined,
		};
	}

	async archiveWriteEntry(
		filePath: string,
		entry: string,
		bytes: Uint8Array,
		opts: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult> {
		const url = await this.#sessionUrl("archive.write");
		url.searchParams.set("path", filePath);
		url.searchParams.set("entry", entry);
		const headers = new Headers(this.#authHeaders());
		if (opts.ifMatch !== undefined) headers.set("if-match", opts.ifMatch);
		const response = await fetch(url, {
			method: "PUT",
			body: bytes,
			headers,
			signal: opts.signal,
		});
		if (!response.ok) throw await toClientError(response);
		const body = (await response.json()) as { etag: string };
		return { etag: body.etag, written: bytes.byteLength };
	}

	async archiveBulkWrite(
		filePath: string,
		entries: Array<{ name: string; bytes: Uint8Array }>,
		opts?: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult> {
		const body = await (await this.session()).archiveBulkWrite(filePath, entries, {
			ifMatch: opts?.ifMatch === "*" ? undefined : opts?.ifMatch,
			signal: opts?.signal,
		});
		return body;
	}

	async #glob(req: T.GlobRequest): Promise<{ paths: RemoteGlobPathEntry[]; truncated: boolean }> {
		const url = await this.#sessionUrl("glob");
		url.searchParams.set("patterns", req.patterns.join(","));
		if (req.paths && req.paths.length > 0) {
			url.searchParams.set("paths[]", req.paths.join(","));
		}
		if (req.includeHidden !== undefined) {
			url.searchParams.set("hidden", String(req.includeHidden));
		}
		if (req.limit !== undefined) {
			url.searchParams.set("limit", String(req.limit));
		}
		if (req.gitignore !== undefined) {
			url.searchParams.set("gitignore", String(req.gitignore));
		}
		if (req.maxDepth !== undefined) {
			url.searchParams.set("max_depth", String(req.maxDepth));
		}
		for (const type of req.types ?? []) {
			url.searchParams.append("types[]", type);
		}
		const response = await fetch(url, {
			signal: req.signal,
			headers: this.#authHeaders(),
		});
		if (!response.ok) throw await toClientError(response);
		return (await response.json()) as { paths: RemoteGlobPathEntry[]; truncated: boolean };
	}

	async glob(req: T.GlobRequest): Promise<T.GlobResult> {
		const result = await this.#glob(req);
		const allowedPrefixes = req.paths && req.paths.length > 0 ? req.paths.map(normalizeScopedPath) : undefined;
		const entries = await Promise.all(
			result.paths.map(async entry => {
				if (!(allowedPrefixes?.some(prefix => isWithinScopedPath(entry.path, prefix)) ?? true)) {
					return null;
				}
				if (req.maxDepth !== undefined && depthOfRelativePath(entry.path) > req.maxDepth) {
					return null;
				}
				let type = coerceGlobEntryType(entry.type ?? entry.kind);
				if (type === null) {
					try {
						const stat = await this.stat(entry.path, { signal: req.signal });
						if (!stat.exists) {
							return null;
						}
						type = stat.kind;
					} catch (error) {
						if (error instanceof NotFoundError) {
							return null;
						}
						throw error;
					}
				}
				if (req.types && req.types.length > 0 && !req.types.includes(type as "file" | "dir" | "symlink")) {
					return null;
				}
				return {
					path: entry.path,
					type,
					size: entry.size,
					modified: entry.mtime,
				};
			}),
		);
		return {
			entries: entries.filter(entry => entry !== null),
			truncated: result.truncated,
		};
	}

	async *grep(req: T.GrepRequest): AsyncGenerator<T.GrepHit, T.GrepSummary> {
		const stream = await (await this.session()).grep(req.pattern, {
			paths: req.paths,
			i: req.ignoreCase,
			gitignore: req.gitignore,
			context: req.contextLines,
			contextBefore: req.contextBefore,
			contextAfter: req.contextAfter,
			maxMatches: req.maxMatches,
			signal: req.signal,
		});
		const iterator = stream[Symbol.asyncIterator]() as AsyncIterator<T.GrepHit, T.GrepSummary>;
		try {
			while (true) {
				const next = await iterator.next();
				if (next.done) {
					return next.value ?? { type: "summary", limitReached: false };
				}
				yield next.value;
			}
		} finally {
			await iterator.return?.();
		}
	}

	async #sessionUrl(endpoint: string): Promise<URL> {
		const session = await this.session();
		return new URL(`/sessions/${session.id}/${endpoint}`, session.client.baseUrl);
	}

	#authHeaders(): Record<string, string> | undefined {
		return this.authToken ? { authorization: `Bearer ${this.authToken}` } : undefined;
	}
}

class RemoteEditBackend implements EditBackend {
	readonly #pipeline: BackendEditPipeline;

	constructor(
		fs: FsBackend,
		private readonly session: () => Promise<RwpSession>,
	) {
		this.#pipeline = new BackendEditPipeline(fs);
	}

	async patch(req: T.EditPatchRequest): Promise<T.EditResult> {
		return await this.#pipeline.patch(req);
	}

	async replace(req: T.EditReplaceRequest): Promise<T.EditResult> {
		return await this.#pipeline.replace(req);
	}

	async editAst(req: T.EditAstRequest): Promise<T.AstEditResult> {
		const ops = getAstEditOps(req);
		const { signal } = req;
		return mapAstEditResult(
			await (await this.session()).editAst(
				{
					ops,
					paths: req.paths,
					language: req.language,
					dryRun: req.dryRun,
				},
				{ signal },
			),
		);
	}

	async *grepAst(req: T.GrepAstRequest): AsyncGenerator<T.GrepAstHit, T.GrepAstSummary> {
		const iterator = (
			await (
				await this.session()
			).grepAst(req.pattern, req.paths, {
				language: req.language,
				strictness: req.strictness,
				limit: req.limit,
				signal: req.signal,
			})
		)[Symbol.asyncIterator]() as AsyncIterator<
			{ path: string; line: number; column: number; end_line: number; end_column: number; text: string },
			T.GrepAstSummary,
			undefined
		>;
		try {
			while (true) {
				const next = await iterator.next();
				if (next.done) {
					return next.value ?? { type: "summary" };
				}
				yield mapGrepAstHit(next.value);
			}
		} finally {
			await iterator.return?.();
		}
	}
}

class RemoteShellBackend implements ShellBackend {
	constructor(private readonly session: () => Promise<RwpSession>) {}

	async *exec(req: T.BashExecRequest): AsyncIterable<T.BashEvent> {
		const { signal, outputStreams, sessionKey, minimizer, ...rest } = req;
		const stream = await (await this.session()).bashExec(
			{
				...rest,
				output_streams: outputStreams,
				session_key: sessionKey,
				minimizer: minimizer
					? {
							enabled: minimizer.enabled,
							aggressive: minimizer.aggressive,
							min_lines: minimizer.minLines,
							context_lines: minimizer.contextLines,
						}
					: undefined,
			},
			{ signal },
		);
		for await (const event of stream) {
			if (event.type === "heartbeat") continue;
			yield toStableBashEvent(event);
		}
	}
}

class RemoteSqliteBackend implements SqliteBackend {
	constructor(
		private readonly client: RwpClient,
		private readonly sessionId: () => Promise<string>,
	) {}

	async read(req: T.ReadDbRequest): Promise<T.ReadDbResponse> {
		const { signal, ...query } = req;
		return await this.client.readDb(await this.sessionId(), query.path, {
			table: query.table ?? undefined,
			key: query.key ?? undefined,
			q: query.q ?? undefined,
			where: query.where ?? undefined,
			order: query.order ?? undefined,
			limit: query.limit ?? undefined,
			offset: query.offset ?? undefined,
			signal,
		});
	}

	async write(req: T.WriteDbRequest): Promise<T.WriteDbResponse> {
		const { signal, ...body } = req;
		return await this.client.writeDb(await this.sessionId(), body, { signal });
	}
}

class RemoteKernelBackend implements KernelBackend {
	constructor(private readonly client: RwpClient) {}

	async put(name: string, cfg: T.KernelConfig): Promise<T.KernelStatus> {
		const { signal, idleTimeoutMs, ...rest } = cfg;
		const body = {
			...rest,
			idle_timeout_ms: idleTimeoutMs,
		};
		await this.client.putEval(name, body, { signal });
		return { name, state: "ready", config: cfg };
	}

	async get(name: string): Promise<T.KernelStatus | null> {
		try {
			const status = await this.client.getEval(name);
			return {
				name,
				state: mapKernelState(status.status),
				config: {
					kind: "eval",
					lang: status.lang,
					transport: status.transport,
					idleTimeoutMs: status.idle_timeout_ms,
				},
			};
		} catch (error) {
			if (error instanceof NotFoundError) return null;
			throw error;
		}
	}

	async *exec(name: string, req: T.KernelExecRequest): AsyncIterable<T.KernelEvent> {
		const { signal, ...body } = req;
		const stream = await this.client.execEval(name, body, { signal });
		for await (const event of stream) {
			yield event;
		}
	}

	async delete(name: string): Promise<void> {
		await this.client.deleteEval(name);
	}
}

class RemoteLspBackend implements LspBackend {
	readonly #configs = new Map<string, T.LspConfig>();

	constructor(
		private readonly client: RwpClient,
		private readonly closeTracker: Set<JsonRpcChannel>,
	) {}

	async put(name: string, cfg: T.LspConfig): Promise<T.LspStatus> {
		const { signal, ...body } = cfg;
		await this.client.putLsp(name, body, { signal });
		this.#configs.set(name, cfg);
		const status = await this.client.getLsp(name, { signal });
		return {
			name,
			state: status.initialized ? "ready" : "starting",
			config: cfg,
			capabilities: status.capabilities as T.LspStatus["capabilities"],
			projectLoaded: status.project_loaded,
			openFiles: status.open_files,
			diagnostics: toLspDiagnostics(status.diagnostics),
		};
	}

	async get(name: string): Promise<T.LspStatus | null> {
		try {
			const status = await this.client.getLsp(name);
			return {
				name,
				state: status.initialized ? "ready" : "starting",
				config: this.#configs.get(name),
				capabilities: status.capabilities as T.LspStatus["capabilities"],
				projectLoaded: status.project_loaded,
				openFiles: status.open_files,
				diagnostics: toLspDiagnostics(status.diagnostics),
			};
		} catch (error) {
			if (error instanceof NotFoundError) return null;
			throw error;
		}
	}

	async openChannel(name: string, opts?: { signal?: AbortSignal }): Promise<JsonRpcChannel> {
		const channel = await createJsonRpcChannel(new URL(`/lsp/${name}`, this.client.baseUrl), opts?.signal);
		this.closeTracker.add(channel);
		return channel;
	}

	async delete(name: string): Promise<void> {
		this.#configs.delete(name);
		await this.client.deleteLsp(name);
	}
}

class RemoteDapBackend implements DapBackend {
	readonly #configs = new Map<string, T.DapConfig>();

	constructor(
		private readonly client: RwpClient,
		private readonly closeTracker: Set<JsonRpcChannel>,
	) {}

	async put(name: string, cfg: T.DapConfig, opts?: { signal?: AbortSignal }): Promise<T.DapStatus> {
		const { signal, ...body } = cfg;
		await this.client.putDap(name, body, { signal: opts?.signal ?? signal });
		this.#configs.set(name, cfg);
		return { name, state: "ready", config: cfg };
	}

	async get(name: string): Promise<T.DapStatus | null> {
		try {
			await this.client.getDap(name);
			return { name, state: "ready", config: this.#configs.get(name) };
		} catch (error) {
			if (error instanceof NotFoundError) return null;
			throw error;
		}
	}

	async openChannel(name: string, opts?: { signal?: AbortSignal }): Promise<JsonRpcChannel> {
		const channel = await createJsonRpcChannel(new URL(`/dap/${name}`, this.client.baseUrl), opts?.signal, {
			allowReverseRequests: true,
		});
		this.closeTracker.add(channel);
		return channel;
	}

	async delete(name: string): Promise<void> {
		this.#configs.delete(name);
		await this.client.deleteDap(name);
	}
}

class RemoteBrowserBackend implements BrowserBackend {
	constructor(
		private readonly client: RwpClient,
		private readonly token?: string,
	) {}

	async put(name: string, cfg: T.BrowserConfig, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus> {
		await this.client.putCdp(name, toCdpHandleConfig(cfg), { signal: opts?.signal ?? cfg.signal });
		await this.client.getCdp(name, { signal: opts?.signal ?? cfg.signal });
		return { name, state: "ready", cdpUrl: this.#proxyWsUrl(name) };
	}

	async get(name: string, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus | null> {
		try {
			await this.client.getCdp(name, { signal: opts?.signal });
			return { name, state: "ready", cdpUrl: this.#proxyWsUrl(name) };
		} catch (error) {
			if (error instanceof NotFoundError) return null;
			throw error;
		}
	}

	async list(opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus[]> {
		const handles = await this.client.listCdp({ signal: opts?.signal });
		return handles.map(handle => ({ name: handle.name, state: "ready", cdpUrl: this.#proxyWsUrl(handle.name) }));
	}

	async wsUrl(name: string, opts?: { signal?: AbortSignal }): Promise<string> {
		await this.client.getCdp(name, { signal: opts?.signal });
		return this.#proxyWsUrl(name);
	}

	async delete(name: string, opts?: { signal?: AbortSignal }): Promise<void> {
		await this.client.deleteCdp(name, { signal: opts?.signal });
	}

	#proxyWsUrl(name: string): string {
		const url = new URL(`/cdp/${encodeURIComponent(name)}`, this.client.baseUrl);
		url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
		if (this.token && !url.searchParams.has("token")) {
			url.searchParams.set("token", this.token);
		}
		return url.toString();
	}
}

export interface RemoteBackendOptions {
	baseUrl: string;
	token?: string;
	sessionId?: string;
	/** Optional remote cwd. When omitted, the server picks its own working directory (typically the SSH user's `$HOME`). */
	cwd?: string;
}

export class RemoteBackend implements Backend {
	readonly kind = "remote" as const;
	readonly fs: FsBackend;
	readonly edit: EditBackend;
	readonly shell: ShellBackend;
	readonly sqlite: SqliteBackend;
	readonly kernel: KernelBackend;
	readonly lsp: LspBackend;
	readonly dap: DapBackend;
	readonly browser: BrowserBackend;
	readonly #client: RwpClient;
	readonly #channels = new Set<JsonRpcChannel>();
	#sessionId: string | null;
	#ownsSession: boolean;
	readonly #cwd: string | undefined;
	#sessionPromise: Promise<RwpSession> | null = null;

	constructor(options: RemoteBackendOptions) {
		this.#client = new RwpClient({
			baseUrl: options.baseUrl,
			token: options.token,
		});
		this.#sessionId = options.sessionId ?? null;
		this.#ownsSession = options.sessionId === undefined;
		this.#cwd = options.cwd;
		this.fs = new RemoteFsBackend(() => this.#getSession(), options.token);
		this.edit = new RemoteEditBackend(this.fs, () => this.#getSession());
		this.shell = new RemoteShellBackend(() => this.#getSession());
		this.sqlite = new RemoteSqliteBackend(this.#client, () => this.#getSessionId());
		this.kernel = new RemoteKernelBackend(this.#client);
		this.lsp = new RemoteLspBackend(this.#client, this.#channels);
		this.dap = new RemoteDapBackend(this.#client, this.#channels);
		this.browser = new RemoteBrowserBackend(this.#client, options.token);
	}

	async #getSession(): Promise<RwpSession> {
		if (this.#sessionPromise) {
			return await this.#sessionPromise;
		}
		this.#sessionPromise = (async () => {
			if (this.#sessionId) {
				return new RwpSession(this.#client, this.#sessionId);
			}
			const created = await this.#client.createSession(this.#cwd ? { cwd: this.#cwd } : {});
			this.#sessionId = created.id;
			return created;
		})().catch(error => {
			this.#sessionPromise = null;
			throw error;
		});
		return await this.#sessionPromise;
	}

	async #getSessionId(): Promise<string> {
		return (await this.#getSession()).id;
	}

	async dispose(): Promise<void> {
		for (const channel of Array.from(this.#channels)) {
			await channel.close().catch(() => undefined);
			this.#channels.delete(channel);
		}
		if (this.#ownsSession && this.#sessionId) {
			await this.#client.deleteSession(this.#sessionId).catch(() => undefined);
		}
	}
}
