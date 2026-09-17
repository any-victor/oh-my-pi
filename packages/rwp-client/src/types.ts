import type { components, paths } from "./generated";

export type RequestHeaders = Headers | Record<string, string> | [string, string][];
export type RequestBody =
	| string
	| ArrayBuffer
	| Uint8Array
	| Blob
	| FormData
	| URLSearchParams
	| ReadableStream<Uint8Array>
	| null;

export type JsonPrimitive = boolean | number | string | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue | undefined };
export interface LspPosition {
	line: number;
	character: number;
}

export interface LspRange {
	start: LspPosition;
	end: LspPosition;
}

export interface LspLocation {
	uri: string;
	range: LspRange;
}

export interface LspDiagnosticRelatedInformation {
	location: LspLocation;
	message: string;
}

export interface LspDiagnostic {
	range: LspRange;
	severity?: 1 | 2 | 3 | 4;
	code?: string | number;
	codeDescription?: { href: string };
	source?: string;
	message: string;
	tags?: number[];
	relatedInformation?: LspDiagnosticRelatedInformation[];
	data?: JsonValue;
}

export type ErrorBody = components["schemas"]["ErrorBody"];
export type SessionEvent = components["schemas"]["SessionEvent"];
export type CreateSessionRequest = components["schemas"]["CreateSessionRequest"];
export type SetCwdRequest = components["schemas"]["SetCwdRequest"];
export type PatchEnvRequest = components["schemas"]["PatchEnvRequest"];
export type CreateSessionResponse = components["schemas"]["CreateSessionResponse"];
export type ReadAstSegment = components["schemas"]["ReadAstSegment"];
export type ReadAstResponse = components["schemas"]["ReadAstResponse"];

export interface GlobPathEntry {
	path: string;
	mtime: number;
	size: number;
}

export interface GlobResponse {
	paths: GlobPathEntry[];
	truncated: boolean;
}

export interface GrepRecord {
	path: string;
	line: number;
	kind: string;
	text: string;
	truncated?: boolean;
}

export interface GrepAstLine {
	path: string;
	line: number;
	column: number;
	end_line: number;
	end_column: number;
	text: string;
}

export type EditReplaceRequest = components["schemas"]["EditReplaceRequest"];
export type Hunk = components["schemas"]["Hunk"];
export type EditPatchRequest = components["schemas"]["EditPatchRequest"];
export type AstOp = components["schemas"]["AstOp"];
export interface EditAstRequest extends Omit<components["schemas"]["EditAstRequest"], "dry_run" | "language"> {
	language?: string;
	dryRun?: boolean;
	dry_run?: boolean;
}
export type EditOp = components["schemas"]["EditOp"];
export type EditResult = components["schemas"]["EditResult"];
export type AstFileChange = components["schemas"]["AstFileChange"];
export type AstEditFileChange = components["schemas"]["AstEditFileChange"];
export type AstEditHunk = components["schemas"]["AstEditHunk"];
export type AstEditResult = components["schemas"]["AstEditResult"];
export type BashExecRequest = components["schemas"]["BashExecRequest"] & {
	output_streams?: "merged" | "split";
	session_key?: string;
	minimizer?: {
		enabled: boolean;
		aggressive?: boolean;
		min_lines?: number;
		context_lines?: number;
	};
};

export type BashRawArtifact =
	| {
			kind: "path";
			path: string;
	  }
	| {
			kind: "bytes";
			bytes: string;
	  };

export type BashEvent =
	| {
			type: "output";
			data: string;
	  }
	| {
			type: "stdout";
			data: string;
	  }
	| {
			type: "stderr";
			data: string;
	  }
	| {
			type: "heartbeat";
	  }
	| {
			type: "exit";
			code?: number | null;
			cancelled: boolean;
			timed_out: boolean;
			minimizer?: {
				minimized: boolean;
				original_lines: number;
				minimized_lines: number;
				omitted_lines: number;
				truncated: boolean;
				raw_artifact?: BashRawArtifact;
			};
	  };

export type DapTransport = components["schemas"]["DapTransport"];
export type NamedHandleConfig = components["schemas"]["NamedHandleConfig"];
export type EvalHandleConfig = Extract<NamedHandleConfig, { kind: "eval" }>;
export type LspHandleConfig = Extract<NamedHandleConfig, { kind: "lsp" }>;
export type DapHandleConfig = Extract<NamedHandleConfig, { kind: "dap" }>;
export type CdpSpawnHandleConfig = Extract<NamedHandleConfig, { kind: "cdp-spawn" }>;
export type CdpAttachHandleConfig = Extract<NamedHandleConfig, { kind: "cdp-attach" }>;
export type CdpHandleConfig = CdpSpawnHandleConfig | CdpAttachHandleConfig;
export type KernelState = components["schemas"]["KernelState"];
export type EvalExecRequest = components["schemas"]["EvalExecRequest"] & {
	cwd?: string;
};
export type EvalStatusResponse = {
	name: string;
	lang: string;
	status: KernelState;
	ref_count: number;
	transport?: "stdio" | "jupyter";
	idle_timeout_ms?: number;
};
export type EvalEvent =
	| {
			type: "stdout";
			data: string;
	  }
	| {
			type: "stderr";
			data: string;
	  }
	| {
			type: "display";
			mime: string;
			data: string;
	  }
	| {
			type: "result";
			text: string;
	  }
	| {
			type: "error";
			ename: string;
			evalue: string;
			traceback: string[];
	  }
	| {
			type: "status";
			state: KernelState;
	  };

export interface LspGetResponse {
	name: string;
	initialized: boolean;
	capabilities: JsonValue;
	project_loaded?: boolean;
	open_files?: string[];
	diagnostics?: Record<string, LspDiagnostic[]>;
	ref_count: number;
	last_active_ms: number;
}

export type CdpHandleResponseKind = components["schemas"]["CdpHandleResponseKind"];
export interface CdpHandleResponse {
	name: string;
	kind: CdpHandleResponseKind;
	ws_url: string;
	ref_count: number;
	last_active_ms: number;
	args?: string[];
	headless?: boolean;
	user_prefs?: Record<string, JsonValue>;
}

export type ReadDbQuery = components["schemas"]["ReadDbQuery"];

export interface SqliteColumn {
	name: string;
	type: string;
}

export interface SqliteTableInfo {
	name: string;
	row_count: number;
	columns: SqliteColumn[];
}

export interface SqliteTablesResponse {
	tables: SqliteTableInfo[];
}

export type SqliteRow = Record<string, JsonValue>;

export interface ReadDbResponse {
	rows: SqliteRow[];
	columns: SqliteColumn[];
	rowid_column?: string | null;
}

export type WriteDbOp = components["schemas"]["WriteDbOp"];
export type WriteDbRequest = components["schemas"]["WriteDbRequest"];
export type WriteDbResponse = components["schemas"]["WriteDbResponse"];

export type ApiPaths = paths;

export interface ReadLinesOptions {
	range?: string;
	maxLines?: number;
	maxBytes?: number;
	signal?: AbortSignal;
}

export interface WriteLinesOptions {
	ifMatch?: string;
	signal?: AbortSignal;
}

export interface WriteBlobOptions {
	ifMatch?: string;
	signal?: AbortSignal;
}

export interface GlobOptions {
	paths?: string[];
	hidden?: boolean;
	limit?: number;
	gitignore?: boolean;
	signal?: AbortSignal;
}

export interface GrepOptions {
	paths?: string[];
	i?: boolean;
	skip?: number;
	gitignore?: boolean;
	context?: number;
	contextBefore?: number;
	contextAfter?: number;
	maxMatches?: number;
	signal?: AbortSignal;
}

export interface GrepAstOptions {
	language?: string;
	strictness?: "smart" | "relaxed" | "strict";
	limit?: number;
	signal?: AbortSignal;
}

export interface ReadBlobOptions {
	range?: string;
	signal?: AbortSignal;
}

export interface ReadDbOptions {
	table?: string;
	key?: string;
	q?: string;
	where?: string;
	order?: string;
	limit?: number;
	offset?: number;
	signal?: AbortSignal;
}

export interface DeleteFileOptions extends RequestOptions {}

export interface MkdirOptions extends RequestOptions {
	recursive?: boolean;
}

export interface RenameOptions extends RequestOptions {
	overwrite?: boolean;
}

export interface RequestOptions {
	signal?: AbortSignal;
}

export interface ClientOptions {
	baseUrl: string | URL;
	fetch?: typeof fetch;
	headers?: RequestHeaders;
	token?: string;
}

export interface ReadLinesResult {
	path: string;
	text: string;
	etag: string;
	totalLines: number;
	truncated?: boolean;
	decorated(): string;
}

export interface ReadBlobResult {
	path: string;
	bytes: Uint8Array;
	etag?: string;
	contentType?: string;
	contentRange?: string;
}

export interface NdjsonWebSocketOptions<TMessage> {
	onMessage?: (message: TMessage, event: MessageEvent<string>) => void;
	onClose?: (event: CloseEvent) => void;
	onError?: (event: Event) => void;
}
