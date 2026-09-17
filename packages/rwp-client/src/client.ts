import { anchoredText } from "./anchors";
import { BadRequestError, EtagMismatchError, NotFoundError, RwpError, toRwpError } from "./errors";
import { type JsonRpcChannel, openJsonRpcChannel } from "./jsonrpc";
import { type JsonWebSocket, ndjsonStream, ndjsonStreamWithReturn, openWebSocket } from "./streams";
import type {
	AstEditResult,
	BashEvent,
	BashExecRequest,
	CdpHandleConfig,
	CdpHandleResponse,
	ClientOptions,
	CreateSessionRequest,
	DapHandleConfig,
	DeleteFileOptions,
	EditAstRequest,
	EditPatchRequest,
	EditReplaceRequest,
	EditResult,
	ErrorBody,
	EvalEvent,
	EvalExecRequest,
	EvalHandleConfig,
	EvalStatusResponse,
	GlobOptions,
	GlobResponse,
	GrepAstLine,
	GrepAstOptions,
	GrepOptions,
	GrepRecord,
	LspGetResponse,
	LspHandleConfig,
	MkdirOptions,
	NdjsonWebSocketOptions,
	PatchEnvRequest,
	ReadAstResponse,
	ReadBlobOptions,
	ReadBlobResult,
	ReadDbOptions,
	ReadDbResponse,
	ReadLinesOptions,
	ReadLinesResult,
	RenameOptions,
	RequestBody,
	RequestHeaders,
	RequestOptions,
	SessionEvent,
	SetCwdRequest,
	SqliteTablesResponse,
	WriteBlobOptions,
	WriteDbRequest,
	WriteDbResponse,
	WriteLinesOptions,
} from "./types";

type QueryValue = string | number | boolean | readonly (string | number | boolean)[] | undefined;

export interface ReadAstOptions extends RequestOptions {
	language?: string;
	range?: string;
	minBodyLines?: number;
	minCommentLines?: number;
}

export interface ImageMetaResponse {
	mime_type: string;
	width?: number;
	height?: number;
	channels?: number;
	has_alpha?: boolean;
}

export interface ListWorkspaceEntry {
	path: string;
	file_type: number;
	mtime?: number;
	size?: number;
}

export interface ListWorkspaceOptions extends RequestOptions {
	path: string;
	maxDepth: number;
	hidden?: boolean;
	gitignore?: boolean;
	collectAgentsMd?: boolean;
	timeoutMs?: number;
}

export interface ListWorkspaceResponse {
	entries: ListWorkspaceEntry[];
	agents_md_files: string[];
	truncated: boolean;
}

type GrepAstSummary = {
	type: "summary";
	parseErrors?: Array<{ file?: string; message: string }>;
	filesSearched?: number;
	limitReached?: boolean;
};

type GrepSummary = {
	type: "summary";
	limitReached: boolean;
	truncated?: boolean;
};
interface RequestInitOptions extends RequestOptions {
	query?: Record<string, QueryValue>;
	headers?: RequestHeaders;
	body?: RequestBody;
}

function stripQuotes(value: string | null): string {
	if (value === null) {
		return "";
	}
	return value.replace(/^"|"$/g, "");
}

function appendQuery(url: URL, query: Record<string, QueryValue>): void {
	for (const [key, value] of Object.entries(query)) {
		if (value === undefined) {
			continue;
		}
		if (Array.isArray(value)) {
			for (const item of value) {
				url.searchParams.append(key, String(item));
			}
			continue;
		}
		url.searchParams.set(key, String(value));
	}
}
function isSummaryRecord(value: unknown): value is { type: "summary" } {
	return typeof value === "object" && value !== null && "type" in value && value.type === "summary";
}

async function readErrorBody(response: Response): Promise<ErrorBody> {
	const fallback = { code: `http-${response.status}`, message: response.statusText } satisfies ErrorBody;
	const contentType = response.headers.get("content-type") ?? "";
	if (!contentType.includes("application/json")) {
		return fallback;
	}
	const body = (await response.json()) as Partial<ErrorBody>;
	if (typeof body.code !== "string" || typeof body.message !== "string") {
		return fallback;
	}
	return {
		code: body.code,
		message: body.message,
		detail: body.detail,
	};
}

export class RwpSession {
	readonly client: RwpClient;
	readonly id: string;

	constructor(client: RwpClient, id: string) {
		this.client = client;
		this.id = id;
	}

	delete(options?: RequestOptions): Promise<void> {
		return this.client.deleteSession(this.id, options);
	}

	setCwd(body: SetCwdRequest, options?: RequestOptions): Promise<void> {
		return this.client.setSessionCwd(this.id, body, options);
	}

	patchEnv(body: PatchEnvRequest, options?: RequestOptions): Promise<void> {
		return this.client.patchSessionEnv(this.id, body, options);
	}

	events(options?: RequestOptions): Promise<AsyncIterableIterator<SessionEvent>> {
		return this.client.getSessionEvents(this.id, options);
	}

	readLines(path: string, options?: ReadLinesOptions): Promise<ReadLinesResult> {
		return this.client.readLines(this.id, path, options);
	}

	readBlob(path: string, options?: ReadBlobOptions): Promise<ReadBlobResult> {
		return this.client.readBlob(this.id, path, options);
	}

	readAst(path: string, options?: ReadAstOptions): Promise<ReadAstResponse> {
		return this.client.readAst(this.id, path, options);
	}

	imageMeta(path: string, options?: RequestOptions): Promise<ImageMetaResponse | null> {
		return this.client.imageMeta(this.id, path, options);
	}

	listWorkspace(options: ListWorkspaceOptions): Promise<ListWorkspaceResponse> {
		return this.client.listWorkspace(this.id, options);
	}

	writeLines(path: string, text: string, options?: WriteLinesOptions): Promise<string> {
		return this.client.writeLines(this.id, path, text, options);
	}

	writeBlob(path: string, bytes: RequestBody, options?: WriteBlobOptions): Promise<string> {
		return this.client.writeBlob(this.id, path, bytes, options);
	}

	archiveBulkWrite(
		path: string,
		entries: Array<{ name: string; bytes: Uint8Array }>,
		options?: WriteBlobOptions,
	): Promise<{ etag: string; written: number }> {
		return this.client.archiveBulkWrite(this.id, path, entries, options);
	}

	deleteFile(path: string, options?: DeleteFileOptions): Promise<void> {
		return this.client.deleteFile(this.id, path, options);
	}

	mkdir(path: string, options?: MkdirOptions): Promise<void> {
		return this.client.mkdir(this.id, path, options);
	}

	rename(from: string, to: string, options?: RenameOptions): Promise<void> {
		return this.client.rename(this.id, from, to, options);
	}

	glob(patterns: string | string[], options?: GlobOptions): Promise<GlobResponse> {
		return this.client.glob(this.id, patterns, options);
	}

	grep(pattern: string, options?: GrepOptions): Promise<AsyncIterableIterator<GrepRecord>> {
		return this.client.grep(this.id, pattern, options);
	}

	grepAst(
		pattern: string,
		paths: string | string[],
		options?: GrepAstOptions,
	): Promise<AsyncIterableIterator<GrepAstLine | GrepAstSummary>> {
		return this.client.grepAst(this.id, pattern, paths, options);
	}

	editReplace(body: EditReplaceRequest, options?: RequestOptions): Promise<EditResult> {
		return this.client.editReplace(this.id, body, options);
	}

	editPatch(body: EditPatchRequest, options?: RequestOptions): Promise<EditResult> {
		return this.client.editPatch(this.id, body, options);
	}

	editAst(body: EditAstRequest, options?: RequestOptions): Promise<AstEditResult> {
		return this.client.editAst(this.id, body, options);
	}

	async bashExec(body: BashExecRequest, options?: RequestOptions): Promise<AsyncIterableIterator<BashEvent>> {
		return this.client.bashExec(this.id, body, options);
	}

	readDb(path: string, options?: ReadDbOptions): Promise<SqliteTablesResponse | ReadDbResponse> {
		return this.client.readDb(this.id, path, options);
	}

	writeDb(body: WriteDbRequest, options?: RequestOptions): Promise<WriteDbResponse> {
		return this.client.writeDb(this.id, body, options);
	}
}

export class RwpClient {
	readonly baseUrl: URL;
	private readonly fetchImpl: typeof fetch;
	private readonly defaultHeaders: RequestHeaders;
	private readonly token?: string;

	constructor(options: ClientOptions) {
		this.baseUrl = new URL(options.baseUrl);
		this.fetchImpl = options.fetch ?? fetch;
		this.token = options.token;
		const defaultHeaders = new Headers(options.headers);
		if (options.token) {
			defaultHeaders.set("authorization", `Bearer ${options.token}`);
		}
		this.defaultHeaders = Object.fromEntries(defaultHeaders.entries());
	}

	async createSession(body: CreateSessionRequest, options?: RequestOptions): Promise<RwpSession> {
		const response = await this.requestJson<{ id: string }>("post", "/sessions", {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
		return new RwpSession(this, response.id);
	}

	async deleteSession(id: string, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("delete", `/sessions/${id}`, options);
	}

	async setSessionCwd(id: string, body: SetCwdRequest, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("put", `/sessions/${id}/cwd`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async patchSessionEnv(id: string, body: PatchEnvRequest, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("patch", `/sessions/${id}/env`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async getSessionEvents(id: string, options?: RequestOptions): Promise<AsyncIterableIterator<SessionEvent>> {
		const response = await this.request("get", `/sessions/${id}/events`, options);
		return this.ndjson<SessionEvent>(response);
	}

	async readLines(id: string, path: string, options?: ReadLinesOptions): Promise<ReadLinesResult> {
		const response = await this.request("get", `/sessions/${id}/read.lines`, {
			query: {
				path,
				range: options?.range,
				max_lines: options?.maxLines,
				max_bytes: options?.maxBytes,
			},
			signal: options?.signal,
		});
		const text = await response.text();
		const etag = stripQuotes(response.headers.get("etag"));
		const totalLines = Number.parseInt(response.headers.get("x-total-lines") ?? "0", 10);
		const decoratedPath = options?.range ? `${path}:${options.range}` : path;
		return {
			path: decoratedPath,
			text,
			etag,
			totalLines: Number.isFinite(totalLines) ? totalLines : 0,
			truncated: response.headers.get("x-truncated") === "true",
			decorated: () => anchoredText({ path: decoratedPath, text, etag }),
		};
	}

	async readBlob(id: string, path: string, options?: ReadBlobOptions): Promise<ReadBlobResult> {
		const response = await this.request("get", `/sessions/${id}/read.blob`, {
			query: { path },
			headers: options?.range ? { range: options.range } : undefined,
			signal: options?.signal,
		});
		return {
			path,
			bytes: new Uint8Array(await response.arrayBuffer()),
			etag: stripQuotes(response.headers.get("etag")) || undefined,
			contentType: response.headers.get("content-type") ?? undefined,
			contentRange: response.headers.get("content-range") ?? undefined,
		};
	}

	readAst(id: string, path: string, options?: ReadAstOptions): Promise<ReadAstResponse> {
		return this.requestJson("get", `/sessions/${id}/read.ast`, {
			query: {
				path,
				language: options?.language,
				range: options?.range,
				min_body_lines: options?.minBodyLines,
				min_comment_lines: options?.minCommentLines,
			},
			signal: options?.signal,
		});
	}

	imageMeta(id: string, path: string, options?: RequestOptions): Promise<ImageMetaResponse | null> {
		return this.requestJson("get", `/sessions/${id}/image_meta`, {
			query: { path },
			signal: options?.signal,
		});
	}

	listWorkspace(id: string, options: ListWorkspaceOptions): Promise<ListWorkspaceResponse> {
		return this.requestJson("get", `/sessions/${id}/list_workspace`, {
			query: {
				path: options.path,
				max_depth: options.maxDepth,
				hidden: options.hidden,
				gitignore: options.gitignore,
				collect_agents_md: options.collectAgentsMd,
				timeout_ms: options.timeoutMs,
			},
			signal: options.signal,
		});
	}

	async deleteFile(id: string, path: string, options?: DeleteFileOptions): Promise<void> {
		await this.requestEmpty("delete", `/sessions/${id}/fs`, {
			query: { path },
			signal: options?.signal,
		});
	}

	async mkdir(id: string, path: string, options?: MkdirOptions): Promise<void> {
		await this.requestEmpty("post", `/sessions/${id}/mkdir`, {
			query: { path, recursive: options?.recursive ?? false },
			signal: options?.signal,
		});
	}

	async rename(id: string, from: string, to: string, options?: RenameOptions): Promise<void> {
		await this.requestEmpty("post", `/sessions/${id}/rename`, {
			query: { from, to, overwrite: options?.overwrite ?? false },
			signal: options?.signal,
		});
	}

	async writeLines(id: string, path: string, text: string, options?: WriteLinesOptions): Promise<string> {
		const response = await this.request("put", `/sessions/${id}/write.lines`, {
			query: { path },
			body: text,
			headers: {
				"content-type": "text/plain; charset=utf-8",
				...(options?.ifMatch ? { "if-match": options.ifMatch } : {}),
			},
			signal: options?.signal,
		});
		return stripQuotes(response.headers.get("etag"));
	}

	async writeBlob(id: string, path: string, bytes: RequestBody, options?: WriteBlobOptions): Promise<string> {
		const response = await this.request("put", `/sessions/${id}/write.blob`, {
			query: { path },
			body: bytes,
			headers: options?.ifMatch ? { "if-match": options.ifMatch } : undefined,
			signal: options?.signal,
		});
		return stripQuotes(response.headers.get("etag"));
	}

	async archiveBulkWrite(
		id: string,
		path: string,
		entries: Array<{ name: string; bytes: Uint8Array }>,
		options?: WriteBlobOptions,
	): Promise<{ etag: string; written: number }> {
		return this.requestJson("put", `/sessions/${id}/archive.bulk_write`, {
			query: { path },
			body: JSON.stringify({
				entries: entries.map(entry => ({
					name: entry.name,
					bytes: Buffer.from(entry.bytes).toString("base64"),
				})),
			}),
			headers: {
				"content-type": "application/json",
				...(options?.ifMatch ? { "if-match": options.ifMatch } : {}),
			},
			signal: options?.signal,
		});
	}

	glob(id: string, patterns: string | string[], options?: GlobOptions): Promise<GlobResponse> {
		return this.requestJson("get", `/sessions/${id}/glob`, {
			query: {
				patterns: Array.isArray(patterns) ? patterns.join(",") : patterns,
				"paths[]": options?.paths?.join(","),
				hidden: options?.hidden,
				limit: options?.limit,
				gitignore: options?.gitignore,
			},
			signal: options?.signal,
		});
	}

	async grep(id: string, pattern: string, options?: GrepOptions): Promise<AsyncIterableIterator<GrepRecord>> {
		const response = await this.request("get", `/sessions/${id}/grep`, {
			query: {
				pattern,
				paths: options?.paths?.join(","),
				i: options?.i,
				skip: options?.skip,
				gitignore: options?.gitignore,
				context: options?.context,
				context_before: options?.contextBefore,
				context_after: options?.contextAfter,
				max_matches: options?.maxMatches,
			},
			signal: options?.signal,
		});
		return ndjsonStreamWithReturn<GrepRecord, GrepSummary>(response.body!, {
			isReturnRecord: value => isSummaryRecord(value),
			mapReturn: value => value as GrepSummary,
		});
	}

	async grepAst(
		id: string,
		pattern: string,
		paths: string | string[],
		options?: GrepAstOptions,
	): Promise<AsyncIterableIterator<GrepAstLine | GrepAstSummary>> {
		const response = await this.request("get", `/sessions/${id}/grep.ast`, {
			query: {
				pattern,
				paths: Array.isArray(paths) ? paths.join(",") : paths,
				language: options?.language,
				strictness: options?.strictness,
				limit: options?.limit,
			},
			signal: options?.signal,
		});
		return ndjsonStreamWithReturn<GrepAstLine | GrepAstSummary, GrepAstSummary>(response.body!, {
			isReturnRecord: value => isSummaryRecord(value),
			mapReturn: value => value as GrepAstSummary,
		});
	}

	editReplace(id: string, body: EditReplaceRequest, options?: RequestOptions): Promise<EditResult> {
		return this.requestJson("post", `/sessions/${id}/edit.replace`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	editPatch(id: string, body: EditPatchRequest, options?: RequestOptions): Promise<EditResult> {
		return this.requestJson("post", `/sessions/${id}/edit.patch`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	editAst(id: string, body: EditAstRequest, options?: RequestOptions): Promise<AstEditResult> {
		return this.requestJson("post", `/sessions/${id}/edit.ast`, {
			body: JSON.stringify({
				ops: body.ops,
				paths: body.paths,
				dry_run: body.dryRun ?? body.dry_run ?? false,
				language: body.language,
			}),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async bashExec(
		id: string,
		body: BashExecRequest,
		options?: RequestOptions,
	): Promise<AsyncIterableIterator<BashEvent>> {
		const response = await this.request("post", `/sessions/${id}/bash.exec`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
		return this.ndjson<BashEvent>(response);
	}

	async readDb(id: string, path: string, options?: ReadDbOptions): Promise<SqliteTablesResponse | ReadDbResponse> {
		return this.requestJson("get", `/sessions/${id}/read.db`, {
			query: {
				path,
				table: options?.table,
				key: options?.key,
				q: options?.q,
				where: options?.where,
				order: options?.order,
				limit: options?.limit,
				offset: options?.offset,
			},
			signal: options?.signal,
		});
	}

	writeDb(id: string, body: WriteDbRequest, options?: RequestOptions): Promise<WriteDbResponse> {
		return this.requestJson("post", `/sessions/${id}/write.db`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	getEval(name: string, options?: RequestOptions): Promise<EvalStatusResponse> {
		return this.requestJson("get", `/eval/${name}`, options);
	}

	async putEval(name: string, body: EvalHandleConfig, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("put", `/eval/${name}`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async deleteEval(name: string, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("delete", `/eval/${name}`, options);
	}

	async execEval(
		name: string,
		body: EvalExecRequest,
		options?: RequestOptions,
	): Promise<AsyncIterableIterator<EvalEvent>> {
		const response = await this.request("post", `/eval/${name}`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
		return this.ndjson<EvalEvent>(response);
	}

	getLsp(name: string, options?: RequestOptions): Promise<LspGetResponse> {
		return this.requestJson("get", `/lsp/${name}`, options);
	}

	putLsp(name: string, body: LspHandleConfig, options?: RequestOptions): Promise<LspGetResponse> {
		return this.requestJson("put", `/lsp/${name}`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async deleteLsp(name: string, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("delete", `/lsp/${name}`, options);
	}

	openLspWebSocket<TIn, TOut>(name: string, callbacks?: NdjsonWebSocketOptions<TOut>): JsonWebSocket<TIn, TOut> {
		return openWebSocket<TIn, TOut>(this.websocketUrl(`/lsp/${name}`), callbacks);
	}
	async openLspChannel(name: string): Promise<JsonRpcChannel> {
		return await openJsonRpcChannel(this.websocketUrl(`/lsp/${name}`));
	}

	async getDap(name: string, options?: RequestOptions): Promise<Response> {
		return this.request("get", `/dap/${name}`, options);
	}

	async putDap(name: string, body: DapHandleConfig, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("put", `/dap/${name}`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async deleteDap(name: string, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("delete", `/dap/${name}`, options);
	}

	openDapWebSocket<TIn, TOut>(name: string, callbacks?: NdjsonWebSocketOptions<TOut>): JsonWebSocket<TIn, TOut> {
		return openWebSocket<TIn, TOut>(this.websocketUrl(`/dap/${name}`), callbacks);
	}
	async openDapChannel(name: string): Promise<JsonRpcChannel> {
		return await openJsonRpcChannel(this.websocketUrl(`/dap/${name}`));
	}

	getCdp(name: string, options?: RequestOptions): Promise<CdpHandleResponse> {
		return this.requestJson("get", `/cdp/${name}`, options);
	}
	listCdp(options?: RequestOptions): Promise<CdpHandleResponse[]> {
		return this.requestJson("get", "/cdp", options);
	}

	async putCdp(name: string, body: CdpHandleConfig, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("put", `/cdp/${name}`, {
			body: JSON.stringify(body),
			headers: { "content-type": "application/json" },
			signal: options?.signal,
		});
	}

	async deleteCdp(name: string, options?: RequestOptions): Promise<void> {
		await this.requestEmpty("delete", `/cdp/${name}`, options);
	}

	openCdpWebSocket<TIn, TOut>(name: string, callbacks?: NdjsonWebSocketOptions<TOut>): JsonWebSocket<TIn, TOut> {
		return openWebSocket<TIn, TOut>(this.websocketUrl(`/cdp/${name}`), callbacks);
	}
	async openCdpChannel(name: string): Promise<JsonRpcChannel> {
		return await openJsonRpcChannel(this.websocketUrl(`/cdp/${name}`));
	}

	private websocketUrl(path: string): URL {
		const url = new URL(path, this.baseUrl);
		url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
		if (this.token && !url.searchParams.has("token")) {
			url.searchParams.set("token", this.token);
		}
		return url;
	}

	private async requestJson<T>(method: string, path: string, options?: RequestInitOptions): Promise<T> {
		const response = await this.request(method, path, options);
		return (await response.json()) as T;
	}

	private async requestEmpty(method: string, path: string, options?: RequestInitOptions): Promise<void> {
		await this.request(method, path, options);
	}

	private async request(method: string, path: string, options?: RequestInitOptions): Promise<Response> {
		const url = new URL(path, this.baseUrl);
		if (options?.query) {
			appendQuery(url, options.query);
		}
		const headers = new Headers(this.defaultHeaders);
		if (options?.headers) {
			new Headers(options.headers).forEach((value, key) => {
				headers.set(key, value);
			});
		}
		const response = await this.fetchImpl(url, {
			method,
			headers,
			body: options?.body,
			signal: options?.signal,
		});
		if (response.ok) {
			return response;
		}
		throw toRwpError(response.status, await readErrorBody(response));
	}

	private ndjson<T>(response: Response): AsyncIterableIterator<T> {
		if (response.body === null) {
			throw new RwpError(500, { code: "empty-body", message: "response body is empty" });
		}
		return ndjsonStream<T>(response.body);
	}
}

export { BadRequestError, EtagMismatchError, NotFoundError, RwpError };
